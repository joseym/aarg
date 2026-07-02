/** LLM wire types shared with the deterministic core's callback bridge and the
 *  `POST /api/llm` route. The wasm exports hand a `CompletionRequest` JSON
 *  string to the injected `llm` callback and expect a `CompletionResponse` JSON
 *  string back; `WasmService` forwards it to `/api/llm`. Mirrors
 *  `aarg-core`'s `llm::types`. */

export type MessageRole = 'user' | 'assistant';

export interface Message {
  role: MessageRole;
  content: string;
  tool_calls?: unknown[];
  tool_results?: unknown[];
  attachments?: unknown[];
}

export interface CompletionRequest {
  model: string;
  max_tokens: number;
  system: string | null;
  messages: Message[];
  temperature: number | null;
  tools?: unknown[];
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
}

export interface CompletionResponse {
  text: string;
  tool_calls?: unknown[];
  model: string;
  stop_reason: string | null;
  usage: TokenUsage;
}

/** Per-tier model ids the wasm exports need as their `models_json` argument. */
export interface Models {
  model: string;
  [tier: string]: string;
}
