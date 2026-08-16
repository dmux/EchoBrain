/**
 * The shapes that cross the IPC boundary.
 *
 * These mirror the DTOs the Rust side serialises in `echobrain-app`. They are
 * deliberately not a re-expression of the Rust domain: `Distance` never appears
 * here, only `similarity`, because the inversion between them happens once, in
 * `echobrain-core`, and a view that could see a distance is a view that could
 * render it backwards.
 */

/** The memory domain has exactly four kinds. The UI does not invent more. */
export type Kind = "fact" | "preference" | "episode" | "snippet";

export const KINDS: readonly Kind[] = ["fact", "preference", "episode", "snippet"] as const;

/** Display labels. Kept here so a badge and a notice cannot disagree. */
export const KIND_LABELS: Record<Kind, string> = {
  fact: "fato",
  preference: "preferência",
  episode: "episódio",
  snippet: "código",
};

/** Whether a written memory is known to be searchable yet. */
export type IndexState = "indexing" | "indexed" | "forgotten";

export interface Memory {
  memoryId: string;
  kind: Kind;
  text: string;
  /** Unix seconds. */
  createdAt: number;
  expiresAt: number | null;
  source: string | null;
  githubLogin?: string | null;
  githubRepo?: string | null;
  rating?: number | null;
  active?: boolean;
}

/**
 * What the library narrows a listing by. Every field is optional, all are "and".
 *
 * Metadata only. This is a filing aid — "which memories came from that
 * repository" — and never a substitute for {@link MemoryGateway.search}, which
 * is the only thing that looks at what a memory means. `textContains` in
 * particular is a literal, case-sensitive substring, not a search.
 */
export interface MemoryFilter {
  kind?: Kind;
  githubRepo?: string;
  githubLogin?: string;
  /** Which client wrote it, e.g. "echobrain-macos". */
  source?: string;
  active?: boolean;
  /** Keeps memories rated at least this highly. Unrated ones are dropped. */
  minRating?: number;
  textContains?: string;
}

/** One page of the library. */
export interface MemoryPage {
  memories: Memory[];
  /**
   * Pass back as `cursor` to continue. `null` is the end — and the **only**
   * signal for it.
   *
   * A short page is not the end: the backend filters after taking the page, so
   * a page can arrive empty with thousands of rows still behind it. Stopping
   * when `memories.length < limit` silently truncates the library.
   */
  nextCursor: string | null;
}

/**
 * An edit to an existing memory. Every field is optional and only the ones
 * present are written — so rating something never risks overwriting its text
 * with a stale copy from the screen.
 */
export interface MemoryUpdate {
  text?: string;
  kind?: Kind;
  /** `null` clears the rating. 1..=5 otherwise; the server rejects the rest. */
  rating?: number | null;
  /** False keeps the memory stored and listed, but out of recall. */
  active?: boolean;
}

export interface Hit {
  memory: Memory;
  /** 0..=1, where 1 is identical. Already inverted from COSINE distance. */
  similarity: number;
}

export interface JournalEntry extends Memory {
  state: IndexState;
}

export interface CaptureResult {
  entry: JournalEntry;
  /**
   * Set when the memory was stored remotely but the local journal write failed.
   * Never a reason to retry the capture — that would pay for a second embedding
   * and store a duplicate.
   */
  journalFailed: string | null;
}

/** One dictation, transcribed but not yet stored. */
export interface Dictation {
  text: string;
  language: string | null;
  /** Length of the recording, so a stray hotkey tap can be told from speech. */
  seconds: number;
}

/** What the speaker wanted: store this, or search for it. */
export type Intent = "record" | "ask";

export const INTENT_LABELS: Record<Intent, string> = {
  record: "guardar",
  ask: "perguntar",
};

/**
 * A composed answer and the memories behind it.
 *
 * `cited` is not optional, and no view may render `text` without it. This is
 * the only text EchoBrain shows that nobody dictated — everything else is a
 * memory played back verbatim — so the sources are what make it checkable
 * rather than something the user has to take on faith.
 */
export interface Answer {
  text: string;
  cited: Hit[];
  /**
   * False when no memory supported the answer. Renders as an empty state, not
   * as a result that happens to have zero sources: "não encontrei nada" is a
   * different outcome, not a weaker one.
   */
  grounded: boolean;
  /**
   * Whether a model was called.
   *
   * With `grounded: false` this separates two outcomes that look identical and
   * cost very differently: nothing cleared the similarity floor (free, no call
   * made) versus the model read the memories and declined (billed).
   */
  consulted: boolean;
}

export interface AskOptions {
  /**
   * Whether to read the answer aloud. The frontend decides, because it is the
   * only side that knows whether the question was typed or spoken.
   */
  speak?: boolean;
  /** Override the similarity floor below which nothing is answered. */
  minSimilarity?: number;
}

export interface SearchOptions {
  kind?: Kind;
  topK?: number;
  /** Drop hits weaker than this, on 0..=1. */
  minSimilarity?: number;
}

export interface CaptureOptions {
  /** Omit to let the classifier guess. */
  kind?: Kind;
  ttlSeconds?: number;
}

/** Where a spoken turn is. Mirrors `Phase` in `echobrain-core`. */
export type ConversationPhase =
  | "idle"
  | "listening"
  | "hearing"
  | "working"
  | "speaking"
  | "follow_up";

export const PHASE_LABELS: Record<ConversationPhase, string> = {
  idle: "parado",
  listening: "ouvindo",
  hearing: "você está falando",
  working: "procurando",
  speaking: "respondendo",
  follow_up: "pode continuar",
};

/** Pushed by Rust on `echobrain://conversation-state`. */
export interface ConversationState {
  phase: ConversationPhase;
  /**
   * Reported honestly, including while the app is speaking — barge-in only
   * works because the microphone never closed, and showing it shut would be
   * describing a microphone that is on.
   */
  microphoneOpen: boolean;
  /** The last thing understood, shown while the answer is still composing. */
  heard: string | null;
  error: string | null;
}

/** A system voice this machine has installed. */
export interface Voice {
  name: string;
  locale: string;
  engine: string;
}

/** The `[voice]` section of config.toml, as the settings screen edits it. */
export interface VoiceSettings {
  ttsEngine: string | null;
  ttsVoice: string | null;
  ttsRate: number;
  /**
   * Bedrock inference profile id. `null` disables answering entirely, which is
   * the default: with no model named, nothing here ever costs a token.
   */
  answerModel: string | null;
  answerMaxTokens: number;
  /** The similarity floor, on 0..=1. Shown as a number, never as a mood. */
  minSimilarity: number;
  voiceThreshold: number;
  /**
   * "allow" or "ignore" — what conversation mode does with a voice that is not
   * the enrolled one. Enrolling sets it to "ignore"; this is how it is undone.
   */
  unknownVoice: string;
}

export interface VoiceProfile {
  enrolledFrom: number;
  enrolledAt: number;
}

/** Which AWS account the app talks to, and as whom. The editable half. */
export interface ConnectionSettings {
  /** The memory API. Signed with SigV4, so it is always https. */
  endpoint: string;
  /**
   * The region of the **API**, which is what SigV4 signs for — not the region
   * of the chosen profile. Getting this wrong fails in a way that reads like a
   * credentials problem and is not one.
   */
  region: string;
  /** Named AWS profile. `null` uses the default credential chain. */
  profile: string | null;
  /** Attribution written onto memories. Never used for authorization. */
  githubLogin: string | null;
}

/** How long the AWS SSO login lasts, and whether it renews itself. */
export interface SsoStatus {
  /**
   * The `[sso-session]` name, or `null` for a profile written the old way —
   * `sso_start_url` directly under `[profile x]`. That shape gets no refresh
   * token, which is what makes {@link renewable} false.
   */
  session: string | null;
  /** Unix seconds, so the countdown can be recomputed without asking again. */
  expiresAt: number;
  /** Seconds remaining; negative once expired. */
  expiresIn: number;
  /**
   * Whether the SDK can renew this token by itself, with no browser. When true
   * the session survives as long as the app is open (up to the session duration
   * configured in IAM Identity Center); when false it simply ends.
   */
  renewable: boolean;
}

/** [`ConnectionSettings`] plus the facts about this machine the screen renders. */
export interface Connection extends ConnectionSettings {
  /**
   * The SSO session behind the selected profile. `null` when the profile does
   * not use SSO or nobody has logged in yet — neither is worth reporting, so
   * the screen shows nothing rather than an alarming blank.
   */
  sso: SsoStatus | null;
  /**
   * Profile names found in `~/.aws/config`. Empty is normal — a machine configured purely through environment variables has no
   * such file — and the screen offers a free-text field instead of a menu.
   */
  profiles: string[];
  /**
   * Environment variables set in the app's process that will override the saved
   * file on the *next* launch. Surfaced because `make run` exports
   * `AWS_PROFILE`: without saying so, the screen would appear to save a profile
   * that silently does not survive a restart.
   */
  envOverrides: string[];
}

/** Distinguishes "you typed something invalid" from "the system failed". */
export class MemoryGatewayError extends Error {
  readonly invalidInput: boolean;

  constructor(message: string, invalidInput = false) {
    super(message);
    this.name = "MemoryGatewayError";
    this.invalidInput = invalidInput;
  }
}
