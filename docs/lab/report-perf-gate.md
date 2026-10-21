# 報告：第五批軌 D——效能閘門（第 1 層 PR-D）

日期：2026-10-21（23:42 開工）。任務書：`docs/lab/parallel-1021e-task.md`「軌 D」。PR：#21。

## 步驟對照表

| 步驟 | 做了什麼 | 證據 |
|---|---|---|
| D-1.1 | `git fetch origin`；`git worktree add ../audit-loupe-gate -b feat/perf-gate bc52503` | `HEAD is now at bc52503 Merge pull request #17`；`rev-parse HEAD` = `bc52503e72914b19479ec0b3412d34a1f60937de` |
| D-1.2 | `backend` 跑 `cargo clippy --all-targets`，記基準 | 主程式 19 個警告；測試 21 個（其中 19 個與主程式重複） |
| D-2 | 比對 SHA-256 後 `git am` | `fe090b09…c18e5c66d`，與 README 相同；套用成 commit `423699d`，改 `.github/workflows/ci.yml`、`backend/src/main.rs`、`backend/src/perf.rs`（+257 行） |
| D-3.1 | `cargo test --locked` | 單元 `117 passed; 0 failed; 3 ignored`；整合 `10 passed; 0 failed; 1 ignored` |
| D-3.2 | `cargo clippy --all-targets` | 主程式 19、測試 21，與 D-1 相同 |
| D-3.3 | `git push -u origin feat/perf-gate`，開 PR 到 `main` | https://github.com/yansheng133/audit-loupe/pull/21 ，標題照任務書，描述第一行「第 1 層 PR-D；請在 0.4.0 發版 PR 之後合併」＋README「做了什麼」表 |
| D-4 | 記 `uptime`、`pgrep`，跑 `cargo test --release --locked perf_gate -- --ignored --nocapture` | 見下方「Mac 結果」；測試 `1 passed`，exit 0 |
| D-5.1 | 等 CI（`gh pr checks 21 --watch`，前景） | `perf (record only)` 成功，6 分 58 秒（15:44:13Z → 15:51:11Z） |
| D-5.2 | `gh run view 39628378527 --log --job 119319646961` 找表格 | 找到，見下方「CI 結果」 |
| D-5.3 | 另兩個 job | `backend tests` 成功 2 分 23 秒；`helm chart` 成功 13 秒 |
| D-6 | 寫本報告，commit、`git push`（不 force） | 本檔 |

## 卡住的地方

無。

## 任務書沒寫但我做了的事

- 這個 shell 的 `PATH` 找不到 `cargo`，每條 cargo 指令前加了 `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`（只影響該指令的 shell，沒改任何設定檔）。
- PR 描述先寫到 session 暫存資料夾的檔案，再用 `gh pr create --body-file` 送出。

## 需要指出的事

- 任務書 D-3.1 只寫「整合 10 通過」，實際另有 **1 個整合測試略過**（`10 passed; 1 ignored`）。0 失敗，照判準算通過。
- D-4 編譯時出現 3 次 `stripping debug info with rust-objcopy failed: signal: 6`（`rust-objcopy` 找不到 `libLLVM.dylib`）。是本機 Rust 工具鏈的問題，只是沒去掉除錯資訊，不影響測試結果；沒處理。
- README 寫的雲端數字（總覽首頁 p50 約 1.0 秒、p95 約 2.0 秒）和這次 CI 的結果（p50 約 0.6 秒、p95 1.5 秒）不同，雲端機器每次跑的結果會有差異。

## Mac 結果（D-4）

開始前 `uptime`：`23:44  up 16 days, 14:28, 7 users, load averages: 12.15 12.34 10.01`

當時有沒有其他軌在編譯：`pgrep -fl 'cargo|rustc'` **沒有結果**（沒有 cargo／rustc 行程）。不過系統負載很高（12），表示有其他工作在跑。

執行時間 23:44:14 → 23:46:34（編譯 38.8 秒，測試 99.95 秒）。

印出的表格（原樣）：

```
### 變更地圖效能閘門（記錄用）

一天 714373 筆（lab 10/20 的 11 倍），寫入 58.7 秒（每秒 12161 筆）。每項 20 次，不含第一次。伺服器端時間，不含網路與瀏覽器。

| 畫面 | p50 (ms) | p95 (ms) | 最慢 (ms) | 回應大小（未壓縮） | 目標 p95 | 結果 |
|---|---|---|---|---|---|---|
| 總覽首頁（全部叢集） | 321 | 952 | 1206 | 161 KB | ≤ 300 ms | 未達標 |
| 總覽首頁（含系統元件） | 343 | 428 | 440 | 106 KB | ≤ 300 ms | 未達標 |
| 總覽首頁（單一叢集，每分鐘時間列） | 344 | 386 | 389 | 232 KB | ≤ 300 ms | 未達標 |
| 關係分頁 | 335 | 361 | 385 | 21 KB | ≤ 300 ms | 未達標 |
| 事件分頁 | 325 | 349 | 358 | 21 KB | ≤ 300 ms | 未達標 |
| 單一物件細節 | 1 | 1 | 1 | 39 KB | ≤ 100 ms | 達標 |
```

## CI 結果（D-5）

Run `39628378527`，PR #21。

| job | 結果 | 耗時 |
|---|---|---|
| perf (record only) | 成功 | 6 分 58 秒（測試本身 167.11 秒） |
| backend tests | 成功 | 2 分 23 秒 |
| helm chart | 成功 | 13 秒 |

`perf (record only)` 印出的表格（原樣，去掉每行的 job 名稱與時間戳記前綴）：

```
### 變更地圖效能閘門（記錄用）

一天 714373 筆（lab 10/20 的 11 倍），寫入 95.7 秒（每秒 7463 筆）。每項 20 次，不含第一次。伺服器端時間，不含網路與瀏覽器。

| 畫面 | p50 (ms) | p95 (ms) | 最慢 (ms) | 回應大小（未壓縮） | 目標 p95 | 結果 |
|---|---|---|---|---|---|---|
| 總覽首頁（全部叢集） | 598 | 1525 | 1528 | 161 KB | ≤ 300 ms | 未達標 |
| 總覽首頁（含系統元件） | 626 | 674 | 719 | 106 KB | ≤ 300 ms | 未達標 |
| 總覽首頁（單一叢集，每分鐘時間列） | 616 | 625 | 628 | 232 KB | ≤ 300 ms | 未達標 |
| 關係分頁 | 609 | 614 | 614 | 21 KB | ≤ 300 ms | 未達標 |
| 事件分頁 | 594 | 601 | 629 | 21 KB | ≤ 300 ms | 未達標 |
| 單一物件細節 | 3 | 3 | 3 | 39 KB | ≤ 100 ms | 達標 |
```

## 一句話

閘門本身能跑，Mac 和 CI 都完成記錄；以每天約 71 萬筆來說，五種整頁畫面在兩邊都沒達到 p95 ≤ 300 ms（Mac 約 350–950 ms、CI 約 600–1500 ms），只有單一物件細節達標。
