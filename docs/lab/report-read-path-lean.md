# 報告：讀取路徑 PR，不含連線池（10/21 第四批，軌 C）

依據：`docs/lab/task-1021d.md`。分支 `perf/read-path-lean`，PR #17。執行時間：2026-10-21 18:19 開始。

**一句話結論**：補丁照步驟套上，測試與 clippy 都通過，連線池的程式已經沒有了。10 倍資料庫的量測結果：回應大小從 108,214 bytes 降到 10,105 bytes；每輪最長的回應時間，`lean` 和 `mainA2` 差 −0.25～+0.84 秒；有寫入那組的 WAL 最大值從 229.3 MB 降到 136.6 MB。

名詞：
- **`mainA2`**：上一批的執行檔 `audit-loupe-mainA`（main＋軌 A 補丁，沒有這個 PR），這次重跑，是為了在同樣的機器負載下比較。
- **`lean`**：這個 PR 的執行檔（gzip、Server-Timing、小表，不含連線池）。
- **WAL**：SQLite 的預寫紀錄檔。寫入先放這裡，之後再「checkpoint」回主資料庫。
- **Server-Timing `db`**：server 回報這個請求花在資料庫的毫秒數。

## 步驟對照表

| 步驟 | 做了什麼 | 證據 | 結果 |
|---|---|---|---|
| C-1 | `gh pr view 16`、`gh pr view 15` 的 state | 都是 `MERGED` | 完成 |
| C-2.1 | `git fetch origin`，`git worktree add ../audit-loupe-read -b perf/read-path-lean origin/main` | origin/main 是 `178e160`（#16 合併） | 完成 |
| C-2.2 | 把任務書搬進新分支 | `mv` 之後，主工作目錄的 `git status` 是空的，新工作目錄有 `?? docs/lab/task-1021d.md` | 完成 |
| C-2.3 | clippy 基準 | `~/audit-loupe-lab/patches/trackC/clippy-main.txt`：主程式 **19**、測試 **21** | 完成 |
| C-3 | 比對 SHA-256，`git am` | `12a50370…6972a`，等於 README；commit `6397d56`（`Cargo.toml`、`api.rs`、`changemap.rs`、`store.rs`） | 完成 |
| C-4.1 | `cargo test --locked` | 單元 116 通過、2 略過；整合 10 通過、1 略過；0 失敗 | 完成，一致 |
| C-4.2 | `cargo clippy --all-targets` | 主程式 19、測試 21，和 C-2 相同 | 完成，沒有變多 |
| C-4.3 | `grep -rn "AL_READ_CONNS\|read_pool" src/` | 沒有結果（結束碼 1） | 完成 |
| C-5.1 | 4 個 commit 的 `git show --stat` | `20197b3` 只動 `report-perf-read-path.md`；`e65c31f`、`e827e82`、`514eecf` 只動 `report-perf-concurrency.md` | 完成，都符合 |
| C-5.2 | 照順序 `git cherry-pick` | `d5d05f8`、`59bebff`、`bc41f4b`、`e580542` | 完成 |
| C-6 | commit 任務書，`git push -u origin perf/read-path-lean`，開 PR | commit `fbaf034`；PR #17，標題照任務書，描述貼 README 的「做了什麼」表 | 完成 |
| C-7.1 | `cargo build --release --locked`，複製成 `bin/audit-loupe-lean` | `Finished release`；`bin/audit-loupe-lean` 18:22 | 完成 |
| C-7.2 | `uptime` | 18:22，load averages `21.88 12.44 8.59`；當時沒有 cargo／rustc／webpack 在跑 | 完成 |
| C-7.3 | 4 組量測，用 `sh -c` 一次跑一行 | 4 組的結束碼都是 0；`results-matrix-{mainA2,lean}-10x-{wno,wyes}.tsv` | 完成 |
| C-7.4 | 刪掉 `work-mainA2-*`、`work-lean-*` | 先確認沒有 server 開著它們，再用 `rm -f` 刪掉這兩種前綴（12 個檔）；剩下的只有上一批的 `work-before-*`、`work-after-*`，沒有動 | 完成 |
| C-8.1–8.2 | 本報告的表格與比較 | 下面兩節 | 完成 |
| C-8.3 | commit、push，表格加進 PR 描述 | 本報告的 commit；PR #17 描述 | 完成 |
| C-8.4 | CI 結果寫進報告再 push | 見「CI 結果」 | 見下方 |

## 卡住的地方

沒有。

## 任務書沒寫但我做了的事

1. **套用前看了補丁改動的檔案**（`git apply --stat --check`，只讀）。
2. **cherry-pick 前用 grep 掃了兩份量測報告**，看有沒有 IP 與 token（只讀；兩份都沒有）。原因是它們會進公開 repo。
3. **C-7.4 刪除前**，用 `ls` 列出要刪的檔，也用 `pgrep` 確認沒有 server 還開著它們（只讀）。
4. **整理數字時用 `python3` 讀 `results-matrix-*.tsv`**（只讀），算出每輪的最短與最長。
5. **C-5.1 用了一個 zsh 迴圈，不符合第 0 節第 9 點。**我用 `for c in 20197b3 e65c31f e827e82 514eecf; do git show --stat … ; done` 一次列出 4 個 commit，沒有照原樣一個一個執行。迴圈裡是 4 個寫死的值，沒有拆變數，4 個 commit 的結果都有正確印出（見步驟對照表 C-5.1）。但這仍然是我自己在 zsh 裡寫的迴圈，第一次交報告時也漏列了，是 Sam 問起後才補上。

## 任務書可能寫錯的地方（沒有造成損害，照做）

- 第 0 節第 2 點寫「不改相依套件的設定」，但補丁本來就改了 `backend/Cargo.toml`：tower-http 加上 `compression-gzip` 功能。README 也寫了這一點。我照 C-3 套用；`Cargo.lock` 沒有變。

## 量測結果（10 倍資料庫，`/api/changemap?day=2026-10-20`，每輪同時發 5 個請求，帶 `Accept-Encoding: gzip`）

機器：18 核心。四組依任務書的順序連續執行；負載是從 C-7.2 的建置之後一路下降。

| 組 | 寫入 | 第 1 輪（秒） | 第 2 輪（秒） | 第 3 輪（秒） | bytes | Server-Timing db（秒，最短–最長） | WAL 最大／結束 | checkpoint_passive | load 開始 → 結束（1 分鐘） |
|---|---|---|---|---|---|---|---|---|---|
| `mainA2-10x-wno` | 沒有 | 1.35–6.47 | 0.39–1.81 | 0.36–1.94 | 108,214／108,209 | —（沒有這個標頭） | 94,792 B／94,792 B | — | 20.61 → 18.50 |
| `lean-10x-wno` | 沒有 | 1.44–6.42 | 0.55–1.83 | 0.42–1.82 | 10,105／10,104 | 1.44–6.42、0.55–1.83、0.42–1.82 | 152,472 B／152,472 B | — | 17.58 → 15.86 |
| `mainA2-10x-wyes` | 有 | 1.40–4.61 | 0.61–2.05 | 0.40–1.82 | 108,214／108,209 | — | 229,306,872 B／229,306,872 B | `0\|580\|580` | 15.23 → 12.26 |
| `lean-10x-wyes` | 有 | 1.54–5.45 | 0.53–1.80 | 0.35–1.64 | 10,107／10,104／10,102 | 1.53–5.45、0.53–1.80、0.35–1.63 | 136,557,432 B／136,557,432 B | `0\|581\|581` | 11.68 → 10.66 |

- 「第 N 輪」是那一輪 5 個請求裡最短與最長的秒數（curl 的 `time_total`）。
- `bytes` 是 curl 收到的大小；同一組裡因為輪次不同，有幾個 bytes 的差異，所以列出出現過的值。
- `checkpoint_passive` 是 `busy|WAL 頁數|已 checkpoint 的頁數`；兩組有寫入的都是全部 checkpoint 完。
- 兩組有寫入的，重播都在 60 秒內跑完（`replay_end finished`）。
- 原始數字：`~/audit-loupe-lab/perf/results-matrix-*.tsv`、`logs/summary-*.txt`、`logs/sample-*.tsv`（不進 repo）。

## 只看數字的比較：`lean` 減 `mainA2`，同樣的寫入條件

| 項目 | 沒有寫入 | 有寫入 |
|---|---|---|
| 回應大小 | 10,105 − 108,214 = −98,109 bytes（−90.7%） | 10,107 − 108,214 = −98,107 bytes（−90.7%） |
| 第 1 輪最長 | 6.42 − 6.47 = −0.05 秒 | 5.45 − 4.61 = +0.84 秒 |
| 第 2 輪最長 | 1.83 − 1.81 = +0.02 秒 | 1.80 − 2.05 = −0.25 秒 |
| 第 3 輪最長 | 1.82 − 1.94 = −0.12 秒 | 1.64 − 1.82 = −0.18 秒 |
| 第 1 輪最短 | 1.44 − 1.35 = +0.09 秒 | 1.54 − 1.40 = +0.14 秒 |
| 第 2 輪最短 | 0.55 − 0.39 = +0.16 秒 | 0.53 − 0.61 = −0.08 秒 |
| 第 3 輪最短 | 0.42 − 0.36 = +0.06 秒 | 0.35 − 0.40 = −0.05 秒 |
| 15 個請求的秒數總和 | 31.73 − 31.22 = +0.51 秒 | 29.82 − 28.99 = +0.83 秒 |
| WAL 最大 | 152,472 − 94,792 = +57,680 B | 136,557,432 − 229,306,872 = −92,749,440 B（−40.4%） |
| 開始時的 load | 17.58 vs 20.61 | 11.68 vs 15.23 |

- `lean` 的 Server-Timing `db` 和 curl 量到的總秒數，每個請求差不到 0.003 秒。
- 四組開始時的 load 都不一樣（20.61 → 11.68），所以兩組之間的機器負載不完全相同。

## CI 結果

PR #17：
- `fbaf034`（補丁＋兩份量測報告＋任務書）：CI 通過。
- `ba06a63`（再加上本報告）：`backend tests` 通過（2 分 9 秒）、`helm chart` 通過（14 秒）。
- `aed79e6`（加上 CI 結果）：`backend tests` 通過（2 分 49 秒）、`helm chart` 通過（15 秒）。
- 之後只補了「任務書沒寫但我做了的事」第 5 點，內容只有文件；這次 push 的 CI 結果寫在 PR 描述，不再改報告。
