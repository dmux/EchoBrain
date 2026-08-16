import type { MemoryGateway } from "./gateway";
import {
  type Answer,
  type AskOptions,
  type CaptureOptions,
  type CaptureResult,
  type Connection,
  type ConnectionSettings,
  type Dictation,
  type Hit,
  type Intent,
  type JournalEntry,
  type Kind,
  type Memory,
  MemoryGatewayError,
  type SearchOptions,
  type Voice,
  type VoiceSettings,
  type VoiceProfile,
  MemoryFilter,
  MemoryPage,
  MemoryUpdate,
} from "./types";

/**
 * An in-process {@link MemoryGateway} for `next dev` and for tests.
 *
 * Its ranking is deliberately crude — token overlap, not embeddings — because
 * it is not pretending to be the real thing. What it does reproduce faithfully
 * are the behaviours the UI has to get right:
 *
 * - results arrive best-first, scored 0..=1;
 * - a fresh capture is `indexing`, not `indexed`, and only flips after a delay,
 *   because DynamoDB vector search is eventually consistent;
 * - `library()` reflects local writes only.
 */
export class FakeGateway implements MemoryGateway {
  private entries: JournalEntry[] = [];
  private nextId = 1;
  private recordingSince: number | null = null;

  /** How long a capture stays `indexing`. The real backend can take ~90 s. */
  constructor(private readonly indexingDelayMs = 4000) {
    for (const seed of SEEDS) {
      this.entries.push({
        memoryId: `mem-${this.nextId++}`,
        kind: seed.kind,
        text: seed.text,
        createdAt: seed.createdAt,
        expiresAt: null,
        source: "echobrain-macos",
        state: "indexed",
      });
    }
  }

  async search(query: string, options?: SearchOptions): Promise<Hit[]> {
    const trimmed = query.trim();
    if (!trimmed) return [];

    const wanted = tokenise(trimmed);
    const topK = options?.topK ?? 5;

    return this.entries
      .filter((entry) => entry.state !== "forgotten")
      .filter((entry) => !options?.kind || entry.kind === options.kind)
      .map((entry) => ({
        memory: entry,
        similarity: overlap(wanted, tokenise(entry.text)),
      }))
      .filter((hit) => hit.similarity > (options?.minSimilarity ?? 0.01))
      .sort((a, b) => b.similarity - a.similarity)
      .slice(0, topK);
  }

  async capture(
    text: string,
    options?: CaptureOptions,
  ): Promise<CaptureResult> {
    const trimmed = text.trim();
    if (!trimmed) {
      throw new MemoryGatewayError("nothing was said", true);
    }

    const entry: JournalEntry = {
      memoryId: `mem-${this.nextId++}`,
      kind: options?.kind ?? classify(trimmed),
      text: trimmed,
      createdAt: Math.floor(Date.now() / 1000),
      expiresAt: options?.ttlSeconds
        ? Math.floor(Date.now() / 1000) + options.ttlSeconds
        : null,
      source: "echobrain-macos",
      state: "indexing",
    };
    this.entries.unshift(entry);

    // The index catching up later, so the UI's `indexing → indexed` transition
    // is exercised during development instead of only in production.
    setTimeout(() => {
      const row = this.entries.find((e) => e.memoryId === entry.memoryId);
      if (row?.state === "indexing") row.state = "indexed";
    }, this.indexingDelayMs);

    return { entry, journalFailed: null };
  }

  async forget(memoryId: string): Promise<void> {
    const row = this.entries.find((e) => e.memoryId === memoryId);
    if (row) row.state = "forgotten";
  }

  async library(limit = 50): Promise<JournalEntry[]> {
    return this.entries
      .filter((entry) => entry.state !== "forgotten")
      .slice(0, limit);
  }

  /**
   * Paginates the way the backend does, including the part that catches people
   * out: the page is taken **before** the filter runs, so a filtered page can
   * come back short — or empty — with rows still to come. A fake that filtered
   * first would let `make ui` bless a scroll that truncates against the real
   * API.
   */
  async listAll(options?: {
    limit?: number;
    cursor?: string | null;
    filter?: MemoryFilter;
  }): Promise<MemoryPage> {
    const rows = this.entries.filter((entry) => entry.state !== "forgotten");
    const limit = options?.limit ?? 25;

    const from = options?.cursor
      ? rows.findIndex((entry) => entry.memoryId === options.cursor) + 1
      : 0;
    const page = rows.slice(from, from + limit);
    const nextCursor =
      from + page.length < rows.length
        ? (page[page.length - 1]?.memoryId ?? null)
        : null;

    const filter = options?.filter ?? {};
    const keep = (entry: Memory) =>
      (filter.kind === undefined || entry.kind === filter.kind) &&
      (filter.githubRepo === undefined || entry.githubRepo === filter.githubRepo) &&
      (filter.githubLogin === undefined || entry.githubLogin === filter.githubLogin) &&
      (filter.source === undefined || entry.source === filter.source) &&
      (filter.active === undefined || (entry.active ?? true) === filter.active) &&
      // Unrated is not badly rated: a floor asks about a judgement that exists.
      (filter.minRating === undefined ||
        (entry.rating != null && entry.rating >= filter.minRating)) &&
      (filter.textContains === undefined || entry.text.includes(filter.textContains));

    return { memories: page.filter(keep), nextCursor };
  }

  async update(memoryId: string, updates: MemoryUpdate): Promise<void> {
    const entry = this.entries.find((e) => e.memoryId === memoryId);
    if (entry) {
      if (updates.text !== undefined) entry.text = updates.text;
      if (updates.kind !== undefined) entry.kind = updates.kind;
      if (updates.rating !== undefined) entry.rating = updates.rating;
      if (updates.active !== undefined) entry.active = updates.active;
    }
  }

  /**
   * Dictation is available in the fake so the microphone flow can be designed
   * in a browser. It "hears" a canned sentence — there is no microphone here,
   * and pretending otherwise would hide that from whoever is styling it.
   */
  async dictationAvailable(): Promise<boolean> {
    return true;
  }

  async startDictation(): Promise<void> {
    this.recordingSince = Date.now();
  }

  async stopDictation(): Promise<Dictation> {
    const seconds = this.recordingSince
      ? (Date.now() - this.recordingSince) / 1000
      : 0;
    this.recordingSince = null;
    // Long enough to exercise wrapping in the capture field.
    return {
      text: "Exemplo de ditado: decidimos manter o journal local como cache, nunca como inventário.",
      language: "pt",
      seconds,
    };
  }

  /** A plausible speech envelope, so the waveform can be tuned without a mic. */
  async audioLevel(): Promise<number> {
    if (!this.recordingSince) return 0;
    const t = (Date.now() - this.recordingSince) / 1000;
    const envelope = 0.35 + 0.3 * Math.sin(t * 3.1) + 0.15 * Math.sin(t * 11.7);
    return Math.min(1, Math.max(0, envelope));
  }

  /** Mirrors `CueIntent` in `echobrain-core`, loosely. Same order of rules. */
  async classifyIntent(text: string): Promise<Intent> {
    const haystack = text.trim().toLowerCase();
    if (!haystack) return "record";
    if (/^(então |ok,? |tá,? |aí |e )?(grava|anota|guarda|lembra que|salva)/.test(haystack)) {
      return "record";
    }
    if (/^(então |ok,? |tá,? |aí |e )?(busca|procura|pesquisa|pergunta|me lembra)/.test(haystack)) {
      return "ask";
    }
    if (haystack.endsWith("?")) return "ask";
    if (
      /^(por que|porque|qual|quais|quando|quem|onde|cadê|como|quanto|o que|será que|você lembra)/.test(
        haystack,
      )
    ) {
      return "ask";
    }
    return "record";
  }

  /**
   * True in the fake so the whole answer flow can be designed in a browser.
   *
   * On a real install this is false until a Bedrock model is configured, and
   * that is not a state the fake can usefully imitate — it would just hide the
   * screens that need building.
   */
  async answeringAvailable(): Promise<boolean> {
    return true;
  }

  /**
   * A canned answer built from whatever the crude search returns.
   *
   * The important behaviours to reproduce are the two the UI branches on: an
   * ungrounded answer carries no sources, and a grounded one always does. The
   * prose is stitched from the memories rather than invented, which is also the
   * most honest thing a fake can do here.
   *
   * A written answer comes back as markdown, because that is what the real one
   * does when `speak` is false — the app renders it through `MarkdownContent`,
   * and a fake that only ever produced one paragraph would let a broken renderer
   * pass `make ui`.
   */
  async ask(question: string, options?: AskOptions): Promise<Answer> {
    const hits = await this.search(question, {
      topK: 5,
      minSimilarity: options?.minSimilarity ?? 0.3,
    });

    // Latency the real path really has: an embedding, a vector search and a
    // model. Without it the UI's thinking state is never visible while it is
    // being designed.
    await new Promise((resolve) => setTimeout(resolve, 900));

    if (hits.length === 0) {
      return {
        text: "Não encontrei nada sobre isso nas suas memórias.",
        cited: [],
        grounded: false,
        consulted: false,
      };
    }

    const text = options?.speak
      ? `Segundo o que você guardou: ${hits[0].memory.text}`
      : [
          `Segundo o que você guardou:`,
          ``,
          ...hits.map((hit) => `- ${hit.memory.text}`),
        ].join("\n");

    return {
      text,
      cited: hits,
      grounded: true,
      consulted: true,
    };
  }

  async speechAvailable(): Promise<boolean> {
    return true;
  }

  /**
   * Silent by design. A browser could speak through `speechSynthesis`, but the
   * real path goes through the macOS system voice over IPC, and imitating it
   * here would make the fake feel more complete than it is.
   */
  async speak(text: string): Promise<void> {
    console.info("[fake] falaria:", text);
    await new Promise((resolve) =>
      setTimeout(resolve, Math.min(4000, text.length * 55)),
    );
  }

  async stopSpeaking(): Promise<void> {}

  private settings: VoiceSettings = {
    ttsEngine: "system",
    ttsVoice: "Luciana",
    ttsRate: 185,
    answerModel: "us.anthropic.claude-opus-5",
    answerMaxTokens: 2048,
    minSimilarity: 0.3,
    voiceThreshold: 0.7,
    unknownVoice: "allow",
  };

  /**
   * False in the fake, and deliberately so.
   *
   * Conversation mode is driven by Rust pushing `echobrain://conversation-state`
   * as a real microphone produces real boundaries. A browser cannot do that,
   * and a fake that pretended to would let the phase UI be designed against a
   * script that never matches what the detector actually does.
   */
  async conversationAvailable(): Promise<boolean> {
    return false;
  }

  async startConversation(): Promise<void> {
    throw new MemoryGatewayError(
      "o modo conversa só funciona no app, não no navegador",
      true,
    );
  }

  async stopConversation(): Promise<void> {}

  async availableVoices(): Promise<Voice[]> {
    return [
      { name: "Luciana", locale: "pt_BR", engine: "system" },
      { name: "Eddy (Português (Brasil))", locale: "pt_BR", engine: "system" },
      { name: "Reed (Português (Brasil))", locale: "pt_BR", engine: "system" },
      { name: "Vitoria", locale: "pt_BR", engine: "polly" },
      { name: "Camila", locale: "pt_BR", engine: "polly" },
    ];
  }

  async getSettings(): Promise<VoiceSettings> {
    return { ...this.settings, voiceThreshold: this.settings.voiceThreshold || 0.7 };
  }

  /** Clamps the same way the Rust side does, so the screen sees real behaviour. */
  async setSettings(settings: VoiceSettings): Promise<VoiceSettings> {
    this.settings = {
      ttsEngine: settings.ttsEngine || "system",
      ttsVoice: settings.ttsVoice || null,
      ttsRate: Math.min(400, Math.max(90, settings.ttsRate)),
      answerModel: settings.answerModel || null,
      answerMaxTokens: Math.min(8192, Math.max(256, settings.answerMaxTokens)),
      minSimilarity: Math.min(1, Math.max(0, settings.minSimilarity)),
      voiceThreshold: Math.min(1, Math.max(0, settings.voiceThreshold)),
      // Same rule as the Rust side: anything unrecognised falls back to
      // allowing every voice, never to silently discarding speech.
      unknownVoice: settings.unknownVoice === "ignore" ? "ignore" : "allow",
    };
    return { ...this.settings };
  }

  async getVoiceProfile(): Promise<VoiceProfile | null> {
    return null;
  }

  async stopAndMeasureVoice(): Promise<number[]> {
    await new Promise(r => setTimeout(r, 400));
    return new Array(256).fill(0.1);
  }

  async saveVoiceProfile(samples: number[][]): Promise<VoiceProfile> {
    await new Promise(r => setTimeout(r, 800));
    return { enrolledFrom: samples.length, enrolledAt: Date.now() / 1000 };
  }

  /**
   * A machine with three profiles and no environment overrides.
   *
   * `envOverrides` is empty rather than populated because the warning it drives
   * has to be *reachable* but not permanent — flip it to `["AWS_PROFILE"]` to
   * design that state, which is what `make run` actually produces.
   */
  private connection: Connection = {
    endpoint: "https://exemplo.execute-api.us-east-1.amazonaws.com/",
    region: "us-east-1",
    profile: "pessoal",
    githubLogin: "dmux",
    profiles: ["default", "pessoal", "trabalho"],
    envOverrides: [],
    // The legacy shape on purpose: it is the state that needs a warning, so it
    // is the one worth being able to see in the browser without an AWS account.
    sso: {
      session: null,
      expiresAt: Math.floor(Date.now() / 1000) + 3 * 3600,
      expiresIn: 3 * 3600,
      renewable: false,
    },
  };

  async getConnection(): Promise<Connection> {
    return { ...this.connection };
  }

  /** Validates the same way the Rust side does, so the screen sees real errors. */
  async setConnection(connection: ConnectionSettings): Promise<Connection> {
    const endpoint = connection.endpoint.trim();
    const region = connection.region.trim();
    if (!endpoint.startsWith("https://")) {
      throw new MemoryGatewayError(
        "o endpoint precisa começar com https:// — a API é assinada com SigV4 e não aceita texto claro",
        true,
      );
    }
    if (!region) {
      throw new MemoryGatewayError(
        "a região não pode ficar vazia: é ela que o SigV4 assina, e sem ela toda chamada falha como se fossem as credenciais",
        true,
      );
    }

    await new Promise((resolve) => setTimeout(resolve, 400));
    this.connection = {
      ...this.connection,
      endpoint,
      region,
      profile: connection.profile?.trim() || null,
      githubLogin: connection.githubLogin?.trim() || null,
    };
    return { ...this.connection };
  }

  async loginSso(profile: string): Promise<void> {
    if (!this.connection.profiles.includes(profile)) {
      throw new MemoryGatewayError(
        `o perfil '${profile}' não existe em ~/.aws/config — crie-o com \`aws configure sso\``,
        true,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
    // A login moves the expiry, which is what the screen re-reads afterwards.
    // Still not renewable: in the browser this stands in for the legacy profile
    // shape, so the warning survives the login — as it does on a real machine.
    this.connection = {
      ...this.connection,
      sso: {
        session: null,
        expiresAt: Math.floor(Date.now() / 1000) + 8 * 3600,
        expiresIn: 8 * 3600,
        renewable: false,
      },
    };
    console.info("[fake] login SSO em", profile);
  }
}

/** Combining diacritical marks, so "memória" and "memoria" tokenise alike. */
const COMBINING_MARKS = /[̀-ͯ]/g;

function tokenise(text: string): Set<string> {
  return new Set(
    text
      .toLowerCase()
      .normalize("NFD")
      .replace(COMBINING_MARKS, "")
      .split(/[^a-z0-9]+/)
      .filter((token) => token.length > 2),
  );
}

/** Jaccard overlap, scaled so a partial match does not read as near-identical. */
function overlap(a: Set<string>, b: Set<string>): number {
  if (a.size === 0 || b.size === 0) return 0;
  let shared = 0;
  for (const token of a) if (b.has(token)) shared++;
  const union = a.size + b.size - shared;
  return union === 0 ? 0 : Math.min(1, (shared / union) * 1.8);
}

/** Mirrors `CueClassifier` in `echobrain-core`, loosely, for realistic fixtures. */
function classify(text: string): Kind {
  const haystack = text.toLowerCase();
  if (/\b(prefiro|eu gosto|não gosto|i prefer|i like|i love)\b/.test(haystack)) {
    return "preference";
  }
  if (/\b(meu nome é|eu moro|eu trabalho|my name is|i live in)\b/.test(haystack)) {
    return "fact";
  }
  return "episode";
}

const DAY = 86_400;
const now = Math.floor(Date.now() / 1000);

/**
 * The fixture corpus, written in **clusters that share vocabulary**.
 *
 * Not decoration. The graph view derives its edges from `search`, and here
 * `search` is token overlap: a corpus of five sentences with nothing in common
 * produces five dots and zero lines, which is a working feature that looks
 * broken every time someone runs `make ui`. So the seeds are grouped the way a
 * real store is — several memories about storage, several about pagination,
 * several about how this project likes to be tested.
 *
 * # The one number that does not transfer
 *
 * Jaccard and cosine agree on *ordering* and disagree completely on *scale*.
 * Against the real index a genuine hit lands near 0.70 and an unrelated memory
 * near 0.62 — a narrow band at the top (measured; see `usecase/answer.rs`).
 * Here a match is near 1.0 and a miss near 0.08. So a similarity floor tuned by
 * eye against this fake would be badly wrong against the deployed index, in
 * either direction. Tune floors against the measurements in `answer.rs`; use
 * this only to see that the wiring works.
 */
const SEEDS: { kind: Kind; text: string; createdAt: number }[] = [
  // Armazenamento e busca vetorial.
  {
    kind: "episode",
    text: "Decidimos usar índice vetorial no DynamoDB em vez de OpenSearch por causa do custo por byte processado.",
    createdAt: now - 21 * DAY,
  },
  {
    kind: "fact",
    text: "O índice vetorial do DynamoDB cobra por byte processado, então um top_k alto custa dinheiro de verdade.",
    createdAt: now - 20 * DAY,
  },
  {
    kind: "episode",
    text: "Descartamos OpenSearch de novo: o custo por byte processado do índice vetorial no DynamoDB ganhou.",
    createdAt: now - 18 * DAY,
  },
  // Paginação da listagem.
  {
    kind: "fact",
    text: "A listagem é paginada por cursor: next_cursor ausente é o único sinal de fim da lista.",
    createdAt: now - 12 * DAY,
  },
  {
    kind: "fact",
    text: "A paginação por cursor da listagem só termina quando next_cursor vem ausente; página curta não é o fim da lista.",
    createdAt: now - 11 * DAY,
  },
  // Arquitetura e testes.
  {
    kind: "preference",
    text: "Prefiro arquitetura hexagonal com a fronteira imposta pelo compilador, não por convenção.",
    createdAt: now - 14 * DAY,
  },
  {
    kind: "preference",
    text: "Prefiro testes que rodam sem rede: a fronteira imposta pelo compilador facilita fingir as portas.",
    createdAt: now - 5 * DAY,
  },
  {
    kind: "preference",
    text: "Prefiro testes que rodam sem rede e sem conta na nuvem.",
    createdAt: now - 1 * DAY,
  },
  // Deliberadamente sozinha: um grafo real tem nós sem vizinho acima do piso,
  // e a tela precisa mostrar isso sem parecer quebrada.
  {
    kind: "episode",
    text: "Na revisão de quinta batemos o martelo: o journal local funciona como cache.",
    createdAt: now - 3 * DAY,
  },
];
