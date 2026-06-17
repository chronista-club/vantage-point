/**
 * Unison WebTransport echo probe (= GUI redesign 北極星 work graph step 2/3、 protocol close)。
 *
 * 目的: **WKWebView (vp-asset://app の secure context) 内で** `@chronista-club/unison-client`
 * TS SDK が実 WebTransport で echo round-trip を成立させられるか検証する。
 *
 * - 2026-06-15 の WKWebView 実証は raw bytes (= Unison frame でない) での接続確認まで。
 * - SDK 自体の framing は club-unison の `tests/integration/webtransport_e2e.test.ts` で実証済。
 * - 本 probe は両者を合流させ「SDK × WKWebView」 の round-trip を閉じる。
 *
 * ## 使い方 (dev)
 *
 * 1. echo server 起動:
 *    `cargo run -p club-unison --example webtransport_echo_server -- '[::1]:4433'`
 *    → stdout の `CERT_HASH=<64 hex>` を控える。
 * 2. vp-app を開き、 DevTools console で:
 *    `await window.vpUnisonEcho('<CERT_HASH>')`
 *    → `{ text: 'hello-from-wkwebview' }` が返れば round-trip 成立。
 *
 * 本番 bundle には載るが window 露出は entry が DEV 時のみ行う (下流の entry 参照)。
 */
import { connect, type ChannelMeta } from "@chronista-club/unison-client";

/**
 * example server (`webtransport_echo_server`) の `echo` channel meta。
 * server 側 `handle_echo` は `Echo` request の `text` をそのまま response で返す。
 */
const EchoMeta = {
  name: "echo",
  backend: "stream",
  from: "client",
  lifetime: "persistent",
  events: [],
  requests: {
    Echo: { request: "EchoReq", response: "EchoResp" },
  },
} as const satisfies ChannelMeta;

/** echo round-trip の結果 (UI / ログ表示用)。 */
export interface EchoProbeResult {
  ok: boolean;
  sent: string;
  reply?: unknown;
  error?: string;
  /** connect → request 完了までの ms。 */
  elapsedMs?: number;
}

/**
 * WKWebView から echo server に WebTransport で接続し、 1 往復させる。
 *
 * @param certHash echo server が印字した leaf cert の SHA-256 hex (= `serverCertificateHashes` pin)
 * @param url      接続先 (省略時 `https://[::1]:4433` = example server の既定)
 */
export async function runUnisonEchoProbe(
  certHash: string,
  url = "https://[::1]:4433",
): Promise<EchoProbeResult> {
  const sent = "hello-from-wkwebview";
  const started = performance.now();
  let client: Awaited<ReturnType<typeof connect>> | undefined;
  try {
    client = await connect({ url, trust: { certHash } });
    const channel = await client.openChannel(EchoMeta);
    const reply = await channel.request("Echo", { text: sent });
    const elapsedMs = Math.round(performance.now() - started);
    const ok =
      typeof reply === "object" &&
      reply !== null &&
      (reply as { text?: unknown }).text === sent;
    console.log("[unison-echo] round-trip", ok ? "OK" : "MISMATCH", reply, `${elapsedMs}ms`);
    return { ok, sent, reply, elapsedMs };
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    console.warn("[unison-echo] failed:", error);
    return { ok: false, sent, error };
  } finally {
    await client?.disconnect("echo probe done").catch(() => undefined);
  }
}
