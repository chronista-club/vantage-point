/**
 * Vantage Agent Types
 */

// JSON-RPC Types
export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: number | string;
  method: string;
  params?: Record<string, unknown>;
}

export interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: number | string | null;
  result?: unknown;
  error?: JsonRpcError;
}

export interface JsonRpcError {
  code: number;
  message: string;
  data?: unknown;
}

// Agent Types
export interface AgentState {
  conversationHistory: ConversationMessage[];
}

export interface ConversationMessage {
  role: "user" | "assistant";
  content: string;
}

// Event Types
export interface ToolExecutingEvent {
  name: string;
}

export interface ToolResultEvent {
  name: string;
  preview: string;
}

export interface ChatResult {
  content: string;
}
