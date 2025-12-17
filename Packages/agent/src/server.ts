#!/usr/bin/env node

/**
 * Vantage Agent Server Entry Point
 */

import { AgentWebSocketServer } from './websocket.js';

const PORT = parseInt(process.env.AGENT_PORT || '8080', 10);

console.log(`[Agent] Starting Vantage Agent Server...`);

const server = new AgentWebSocketServer(PORT);

// Handle shutdown gracefully
process.on('SIGINT', () => {
  console.log('\n[Agent] Shutting down server...');
  server.close();
  process.exit(0);
});

process.on('SIGTERM', () => {
  console.log('[Agent] Received SIGTERM, shutting down...');
  server.close();
  process.exit(0);
});

console.log(`[Agent] Server ready on ws://localhost:${PORT}`);