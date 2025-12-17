/**
 * WebSocket Server for Vantage Agent
 *
 * Handles bidirectional communication with TUI
 */

import { WebSocketServer, WebSocket } from 'ws';
import { EventEmitter } from 'events';

interface AgentMessage {
  type: 'query' | 'response' | 'error' | 'status';
  data: any;
  id?: string;
}

export class AgentWebSocketServer extends EventEmitter {
  private wss: WebSocketServer;
  private clients: Set<WebSocket> = new Set();
  private port: number;

  constructor(port: number = 8080) {
    super();
    this.port = port;
    this.wss = new WebSocketServer({ port });
    this.setupServer();
  }

  private setupServer() {
    this.wss.on('connection', (ws: WebSocket) => {
      console.log(`[Agent] Client connected`);
      this.clients.add(ws);

      // Send initial status
      this.sendMessage(ws, {
        type: 'status',
        data: { connected: true, ready: true }
      });

      ws.on('message', async (data: Buffer) => {
        try {
          const message = JSON.parse(data.toString()) as AgentMessage;
          console.log(`[Agent] Received message:`, message);

          // Handle different message types
          switch (message.type) {
            case 'query':
              await this.handleQuery(ws, message);
              break;
            default:
              console.warn(`[Agent] Unknown message type: ${message.type}`);
          }
        } catch (error) {
          console.error(`[Agent] Error handling message:`, error);
          this.sendError(ws, error as Error);
        }
      });

      ws.on('close', () => {
        console.log(`[Agent] Client disconnected`);
        this.clients.delete(ws);
      });

      ws.on('error', (error) => {
        console.error(`[Agent] WebSocket error:`, error);
        this.clients.delete(ws);
      });
    });

    console.log(`[Agent] WebSocket server listening on port ${this.port}`);
  }

  private async handleQuery(ws: WebSocket, message: AgentMessage) {
    try {
      // TODO: Integrate with Claude Agent SDK
      // For now, send a mock response
      const response: AgentMessage = {
        type: 'response',
        id: message.id,
        data: {
          content: `Received query: ${JSON.stringify(message.data)}`,
          timestamp: new Date().toISOString()
        }
      };

      this.sendMessage(ws, response);
    } catch (error) {
      this.sendError(ws, error as Error, message.id);
    }
  }

  private sendMessage(ws: WebSocket, message: AgentMessage) {
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(message));
    }
  }

  private sendError(ws: WebSocket, error: Error, id?: string) {
    this.sendMessage(ws, {
      type: 'error',
      id,
      data: {
        message: error.message,
        stack: error.stack
      }
    });
  }

  public broadcast(message: AgentMessage) {
    this.clients.forEach(client => {
      this.sendMessage(client, message);
    });
  }

  public close() {
    this.wss.close();
  }
}