// mem_1Cg1no4SEaGkCYaF3VtCDk
import { expect, it } from "vitest";
import { createGearCache } from "./midi-gear-loader";
it("does not fetch until requested and shares an in-flight/cache entry", async () => {
 let reads = 0;
 const cache = createGearCache(async (id: string) => { reads++; return id; });
 expect(reads).toBe(0);
 expect(await Promise.all([cache("minilab"), cache("minilab")])).toEqual(["minilab", "minilab"]);
 expect(await cache("minilab")).toBe("minilab");
 expect(reads).toBe(1);
});
it("turns missing/broken assets into fallback and does not accept arbitrary paths", async () => {
 let reads = 0;
 const cache = createGearCache(async () => { reads++; throw Error("bad asset"); });
 expect(await cache("../x")).toBeNull();
 expect(reads).toBe(0);
 expect(await cache("maru")).toBeNull();
 expect(await cache("maru")).toBeNull();
 expect(reads).toBe(1);
});
