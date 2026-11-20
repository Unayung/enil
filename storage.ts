// 憑證檔（refreshToken、E2EE 私鑰）。取代 linejs 的 FileStorage，它有三個會咬人的地方：
//   1. writeFile 原地覆寫：rotate 寫到一半掛掉 → 留下半截或空檔 → 只能重掃 QR
//   2. 寫入錯誤丟給 resolve 吞掉，讀檔失敗當成 {}：憑證沒了也不會有任何錯誤
//   3. 權限跟 umask 走，實際看到 0644
// 這裡資料放記憶體、寫入是「暫存檔 → fsync → rename」，權限固定 0600，壞檔直接丟錯不蓋掉。
// ponytail: 只在啟動時讀一次。同一時間只有一個 process 持有 session（見 CLAUDE.md），不必重讀。
import { chmodSync, closeSync, existsSync, fsyncSync, openSync, readFileSync, renameSync, writeSync } from "node:fs";
import { BaseStorage, type Storage } from "@evex/linejs/storage";

type Data = Record<Storage["Key"], Storage["Value"]>;

export class SafeFileStorage extends BaseStorage {
  #data: Data;

  constructor(private path: string) {
    super();
    if (!existsSync(path)) {
      this.#data = {};
      return;
    }
    // 壞檔不能當成空的：那等於默默丟掉憑證，下一次寫入還會把殘骸蓋掉。
    this.#data = JSON.parse(readFileSync(path, "utf8") || "{}");
    chmodSync(path, 0o600);
  }

  #flush() {
    const tmp = `${this.path}.tmp`;
    const fd = openSync(tmp, "w", 0o600);
    try {
      writeSync(fd, JSON.stringify(this.#data));
      fsyncSync(fd); // 不 fsync 的話，斷電後 rename 可能指到還沒落地的空檔
    } finally {
      closeSync(fd);
    }
    renameSync(tmp, this.path);
  }

  async get(key: Storage["Key"]) { return this.#data[key]; }
  async getAll() { return { ...this.#data }; }
  async set(key: Storage["Key"], value: Storage["Value"]) { this.#data[key] = value; this.#flush(); }
  async delete(key: Storage["Key"]) { delete this.#data[key]; this.#flush(); }
  async clear() { this.#data = {}; this.#flush(); }
  async migrate(storage: BaseStorage) {
    for (const [k, v] of Object.entries(this.#data)) await storage.set(k, v);
  }
}
