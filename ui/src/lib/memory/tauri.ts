import { invoke } from "@tauri-apps/api/core";

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
  MemoryGatewayError,
  type SearchOptions,
  type Voice,
  VoiceSettings,
  VoiceProfile,
  MemoryFilter,
  MemoryPage,
  MemoryUpdate,
} from "./types";

/**
 * The real {@link MemoryGateway}: Tauri IPC to the Rust core, which reaches the
 * deployed API over HTTP + SigV4.
 *
 * Nothing here knows about AWS. The command names are the whole contract, and
 * they match `#[tauri::command]` functions in `echobrain-app`.
 */
export class TauriGateway implements MemoryGateway {
  async search(query: string, options?: SearchOptions): Promise<Hit[]> {
    return call<Hit[]>("search_memories", {
      query,
      kind: options?.kind ?? null,
      topK: options?.topK ?? null,
      minSimilarity: options?.minSimilarity ?? null,
    });
  }

  async capture(
    text: string,
    options?: CaptureOptions,
  ): Promise<CaptureResult> {
    return call<CaptureResult>("capture_memory", {
      text,
      kind: options?.kind ?? null,
      ttlSeconds: options?.ttlSeconds ?? null,
    });
  }

  async forget(memoryId: string): Promise<void> {
    await call<null>("forget_memory", { memoryId });
  }

  async library(limit = 50): Promise<JournalEntry[]> {
    return call<JournalEntry[]>("library", { limit });
  }

  async listAll(options?: {
    limit?: number;
    cursor?: string | null;
    filter?: MemoryFilter;
  }): Promise<MemoryPage> {
    return call<MemoryPage>("list_memories", {
      limit: options?.limit,
      // `null` and `undefined` both arrive as an absent argument, which is what
      // "start from the beginning" means on the Rust side.
      cursor: options?.cursor ?? undefined,
      filter: options?.filter,
    });
  }

  async update(memoryId: string, updates: MemoryUpdate): Promise<void> {
    await call<null>("update_memory", { memoryId, updates });
  }

  async dictationAvailable(): Promise<boolean> {
    return call<boolean>("dictation_available", {});
  }

  async startDictation(): Promise<void> {
    await call<null>("start_dictation", {});
  }

  async stopDictation(): Promise<Dictation> {
    return call<Dictation>("stop_dictation", {});
  }

  async audioLevel(): Promise<number> {
    return call<number>("audio_level", {});
  }

  async classifyIntent(text: string): Promise<Intent> {
    return call<Intent>("classify_intent", { text });
  }

  async answeringAvailable(): Promise<boolean> {
    return call<boolean>("answering_available", {});
  }

  async ask(question: string, options?: AskOptions): Promise<Answer> {
    return call<Answer>("ask_memories", {
      question,
      speak: options?.speak ?? false,
      minSimilarity: options?.minSimilarity ?? null,
    });
  }

  async speechAvailable(): Promise<boolean> {
    return call<boolean>("speech_available", {});
  }

  async speak(text: string): Promise<void> {
    await call<null>("speak", { text });
  }

  async stopSpeaking(): Promise<void> {
    await call<null>("stop_speaking", {});
  }

  async conversationAvailable(): Promise<boolean> {
    return call<boolean>("conversation_available", {});
  }

  async startConversation(): Promise<void> {
    await call<null>("start_conversation", {});
  }

  async stopConversation(): Promise<void> {
    await call<null>("stop_conversation", {});
  }

  async availableVoices(): Promise<Voice[]> {
    return call<Voice[]>("available_voices", {});
  }

  async getSettings(): Promise<VoiceSettings> {
    return call<VoiceSettings>("get_settings", {});
  }

  async setSettings(settings: VoiceSettings): Promise<VoiceSettings> {
    // The key must match the Rust parameter name — `set_settings(state,
    // settings)`. It said `update`, and Tauri rejects the call while binding
    // arguments, before the command body runs: no log, no save, and "Testar
    // voz" never reached `speak` because it awaits this first.
    return call("set_settings", { settings });
  }

  async getVoiceProfile(): Promise<VoiceProfile | null> {
    return call("get_voice_profile", {});
  }

  async stopAndMeasureVoice(): Promise<number[]> {
    return call("stop_and_measure_voice", {});
  }

  async saveVoiceProfile(samples: number[][]): Promise<VoiceProfile> {
    return call("save_voice_profile", { samples });
  }

  async getConnection(): Promise<Connection> {
    return call<Connection>("get_connection", {});
  }

  async setConnection(connection: ConnectionSettings): Promise<Connection> {
    // Same rule as `set_settings` above: the key is the Rust parameter name,
    // and a mismatch is rejected while binding arguments — before the command
    // body runs, so nothing is logged and nothing is saved.
    return call("set_connection", { connection });
  }

  async loginSso(profile: string): Promise<void> {
    await call<null>("login_sso", { profile });
  }
}

/**
 * The Rust side returns a tagged error so the UI can tell a bad query from a
 * broken backend without parsing message strings.
 */
interface IpcError {
  message: string;
  invalidInput: boolean;
}

async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    if (isIpcError(raw)) {
      throw new MemoryGatewayError(raw.message, raw.invalidInput);
    }
    throw new MemoryGatewayError(
      raw instanceof Error ? raw.message : String(raw),
    );
  }
}

function isIpcError(value: unknown): value is IpcError {
  return (
    typeof value === "object" &&
    value !== null &&
    "message" in value &&
    "invalidInput" in value
  );
}
