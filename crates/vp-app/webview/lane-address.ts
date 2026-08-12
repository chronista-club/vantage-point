/**
 * lane address の**分解** — repo と lane 名を取り出す唯一の場所。
 *
 * ## ⚠️ 組み立てはここには無い
 *
 * address 文字列を**作る**のは daemon（`LaneAddress::canonical`）だけで、client は
 * `LaneInfo.address.key` をそのまま運ぶ。ここにあるのは**受け取った文字列から部品を
 * 取り出す**側だけ。
 *
 * ## ⚠️ なぜ 1 箇所に畳んだか
 *
 * 同じ分解が webview に 3 つあり（`LaneHeader.laneShortName` / `entry.laneNameFromAddress` /
 * `lane-panes.boardKeyOf`）、どれも `/sub/` `/wing/` を**探す**実装だった。address が
 * `<repo>/lane/<name>` になった瞬間、3 つとも一致しなくなり:
 *
 * - ヘッダに `vantage-point/lane/sampler` が丸ごと出る
 * - board が全 lane で Main のキーに集約される
 *
 * という**無音の劣化**を同時に起こす。分節を探すのではなく **最後の分節が lane 名**という
 * 不変条件で取れば、世代を問わず正しい（`<repo>/lane/<name>` / 旧 `<repo>/sub/<name>` /
 * 旧 `<repo>/wing/<name>` / 旧 `<repo>/<name>` のすべて）。
 */

/**
 * Main lane の**予約名（識別子）**。⚠️ Rust `vp_paths::ROOT_LANE_NAME` と同値。
 *
 * ⚠️ **表示名とは別物**。address / disk / env に出るのはこちらで、UI に出す語は
 * [`MAIN_DISPLAY_NAME`]。混ぜると「識別子を変えたら表示も変わる / その逆」になる。
 */
export const MAIN_LANE_NAME = "root";

/**
 * Main lane の**表示名**（Main/Sub の語彙）。
 *
 * ⚠️ 予約名が `root` のまま UI だけ `main` と呼ぶ、という**意図的な非対称**。識別子を
 * 変えるには migration が要るが、表示は語彙の問題なので独立に決められる。
 */
export const MAIN_DISPLAY_NAME = "main";

/** 旧世代の予約名。address / env に残るので受理する。 */
const LEGACY_MAIN_NAMES = ["lead", "conductor"];

/** address の repo 部（先頭分節）。取れなければ空文字。 */
export function repoOfAddress(address: string): string {
	return address.split("/")[0] ?? "";
}

/**
 * address の lane 名（**最後の分節**）。取れなければ空文字。
 *
 * ⚠️ 分節を「探す」のではなく「最後を取る」。`lane` / `sub` / `wing` のどれが挟まっても、
 * 挟まらなくても同じ答えになる。
 */
export function laneNameOfAddress(address: string): string {
	const parts = address.split("/");
	// ⚠️ 分節が 1 つ = repo だけの文字列で、**address ではない**。lane 名は無いと答える
	// （board は「未知形は Main に倒す」流儀なので、ここで repo 名を lane 名と誤らせない）。
	return parts.length >= 2 ? (parts.pop() ?? "") : "";
}

/**
 * その address は Main lane か。旧世代の予約名（`root` / `lead`）も Main とみなす。
 */
export function isMainAddress(address: string): boolean {
	const name = laneNameOfAddress(address);
	return name === MAIN_LANE_NAME || LEGACY_MAIN_NAMES.includes(name);
}

/**
 * Sub lane の名前。Main なら `null`。
 *
 * 「Main を `null` で表す」のは board / pane 側の既存の流儀に合わせたもの
 * （`boardKey(repo, null)` が Main のキー）。
 */
export function subNameOfAddress(address: string): string | null {
	if (isMainAddress(address)) return null;
	const name = laneNameOfAddress(address);
	return name === "" ? null : name;
}
