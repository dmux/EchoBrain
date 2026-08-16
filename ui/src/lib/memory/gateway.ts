import type {
  Answer,
  AskOptions,
  CaptureOptions,
  CaptureResult,
  Connection,
  ConnectionSettings,
  Dictation,
  Hit,
  Intent,
  JournalEntry,
  SearchOptions,
  Voice,
  VoiceSettings,
  VoiceProfile,
  MemoryFilter,
  MemoryPage,
  MemoryUpdate,
} from "./types";

/**
 * The frontend's port onto the memory layer.
 *
 * The same hexagon the Rust side uses, mirrored here for the same reason: no
 * component should know whether its data arrives over Tauri IPC, from fixtures,
 * or one day from an HTTP endpoint. Components depend on this interface;
 * adapters implement it.
 *
 * The immediate payoff is that the whole UI can be built and reviewed in
 * `next dev` against {@link FakeGateway}, with no Rust toolchain, no AWS
 * account and no cost — which is where interface work actually happens.
 */
export interface MemoryGateway {
  /** Semantic search. Results arrive best-first. */
  search(query: string, options?: SearchOptions): Promise<Hit[]>;

  /** Store something. Returns the entry as it will appear in the library. */
  capture(text: string, options?: CaptureOptions): Promise<CaptureResult>;

  /** Delete a memory, and tombstone it locally. */
  forget(memoryId: string): Promise<void>;

  /**
   * What this device has written.
   *
   * Not an inventory, and not the same thing as {@link listAll}: this reads the
   * local journal, so it also knows what is still *indexing* — a memory written
   * seconds ago that the vector index has not caught up with. That is the whole
   * reason it survived the arrival of `GET /memories`, which cannot report a
   * state it does not track.
   *
   * The cost is coverage: memories captured on another device are absent, and
   * server-side TTL expiries happen without notice. Any view built on it has to
   * say so rather than imply completeness.
   */
  library(limit?: number): Promise<JournalEntry[]>;

  /**
   * One page of everything in the namespace, newest first.
   *
   * Unlike {@link library}, this is a true inventory: it reads the API, so
   * memories written by the MCP server or by an agent through the Lambda are
   * here too. That is what makes it the right screen to manage from.
   *
   * Read {@link MemoryPage.nextCursor} to continue, and **only** that — a short
   * page means the filter dropped rows, not that the list ended.
   */
  listAll(options?: {
    limit?: number;
    cursor?: string | null;
    filter?: MemoryFilter;
  }): Promise<MemoryPage>;

  /** Update a memory's text, kind, rating, active status, etc. */
  update(memoryId: string, updates: MemoryUpdate): Promise<void>;

  /**
   * Whether dictation can be offered at all.
   *
   * False when the ~570 MB speech model is not installed. The microphone is
   * hidden rather than shown broken — a button that always fails is worse than
   * no button.
   */
  dictationAvailable(): Promise<boolean>;

  /** Open the microphone. The macOS permission prompt appears on first call. */
  startDictation(): Promise<void>;

  /**
   * Stop and transcribe. Returns the text rather than storing it: transcription
   * is fallible, so the user sees what was heard before it becomes a memory.
   */
  stopDictation(): Promise<Dictation>;

  /** Input level on 0..=1, for the waveform. Polled, so it must stay cheap. */
  audioLevel(): Promise<number>;

  /**
   * Whether an utterance should be stored or searched for.
   *
   * A heuristic, and wrong sometimes — which is why the UI shows it as an
   * editable chip before acting on it, exactly as it does with the memory kind.
   */
  classifyIntent(text: string): Promise<Intent>;

  /**
   * Whether questions can be answered at all.
   *
   * False when no answering model is configured. Answering is the one feature
   * that costs money per use, so running without it is a supported state:
   * capture and search keep working and the ask path is hidden.
   */
  answeringAvailable(): Promise<boolean>;

  /**
   * Search the memories and compose an answer from them.
   *
   * The returned {@link Answer} carries the memories it drew on. Rendering the
   * text without them is not an option the UI should have.
   */
  ask(question: string, options?: AskOptions): Promise<Answer>;

  /** Whether answers can be read aloud — false with no pt-BR system voice. */
  speechAvailable(): Promise<boolean>;

  /** Read arbitrary text aloud. Resolves when the audio finishes. */
  speak(text: string): Promise<void>;

  /**
   * Silence whatever is being said, and drop anything queued behind it.
   *
   * Safe to call when nothing is speaking: barge-in fires the moment the
   * microphone hears the user, without first checking.
   */
  stopSpeaking(): Promise<void>;

  /**
   * Whether hands-free conversation can be offered.
   *
   * Needs the recogniser and a continuous microphone. Speech and answering are
   * not required: without a voice the turn is shown rather than spoken.
   */
  conversationAvailable(): Promise<boolean>;

  /**
   * Open the microphone and keep it open, taking turns until stopped.
   *
   * The microphone light stays on for the whole conversation — which is why
   * this is a mode the user turns on, never something started for them.
   */
  startConversation(): Promise<void>;

  stopConversation(): Promise<void>;

  /** The pt-BR voices installed on this machine. */
  availableVoices(): Promise<Voice[]>;

  getSettings(): Promise<VoiceSettings>;

  /**
   * Persist the settings and apply what can change without a restart.
   *
   * Returns what was actually applied — values are clamped on the Rust side,
   * so the screen must render the result rather than what it sent.
   */
  setSettings(settings: VoiceSettings): Promise<VoiceSettings>;

  getVoiceProfile(): Promise<VoiceProfile | null>;
  stopAndMeasureVoice(): Promise<number[]>;
  saveVoiceProfile(samples: number[][]): Promise<VoiceProfile>;

  /** The connection settings, plus the profile list and env overrides. */
  getConnection(): Promise<Connection>;

  /**
   * Point the app at another API, region or AWS profile, without a restart.
   *
   * Resolving succeeds locally, so this returning does **not** prove the
   * account is reachable — a wrong profile or an expired session surfaces on
   * the next search. {@link loginSso} is the remedy.
   */
  setConnection(connection: ConnectionSettings): Promise<Connection>;

  /**
   * Refresh the SSO session for a profile, via the AWS CLI.
   *
   * The profile has to already exist in `~/.aws/config`; creating one is
   * `aws configure sso`'s job.
   */
  loginSso(profile: string): Promise<void>;
}
