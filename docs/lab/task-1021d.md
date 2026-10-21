# 任務書：10/21 第四批——讀取路徑 PR（拿掉連線池）

寫作：2026-10-21。Sam 已同意：讀取連線池量下來沒有幫助，拿掉，其餘（gzip、Server-Timing、小表）保留並開 PR。只有一軌。

## 0. 照任務書執行（Sam 10/21 要求，違反就是不合格）

1. **只做本任務書列出的步驟，照編號順序做。**每一步做完，在報告的「步驟對照表」填：步驟編號、做了什麼、證據。
2. **任務書沒寫的動作一律不做**，包括但不限於：
   - 安裝常駐服務、排程或背景程式（包括「暫時的」背景監看）。
   - 改 CI、lint、建置、相依套件的設定。
   - 改程式、改補丁內容。
   - 刪除檔案或分支（步驟有寫的除外；自己產生的錯檔也不要刪，寫進報告）。
   - force-push、合併、打 tag、改寫歷史。
   - 碰任何叢集、在系統層級安裝任何東西。
3. **照做行不通、或任務書沒寫到的情況**：停下那一步，讓程式維持可編譯、測試會過的狀態，在報告的「卡住的地方」寫清楚，跳到下一個不依賴它的步驟。**不要自己換做法、不要擴大範圍。**
4. **發現任務書寫錯**：照字面做會造成損害就停下來；不會就照做。兩種情況都要在報告指出。
5. 硬性限制照舊：不印也不存 token、密碼、IP；Mac 不用 sudo；不開 kubeconfig、`env.sh`、`~/audit-loupe-lab/perf/tokens` 的內容；`~/audit-loupe-lab/overnight/`、`recordings/`、`perf/pristine-*.db` 唯讀；本機服務只綁 127.0.0.1；CLI 不合併 PR。
6. 補丁先比對 SHA-256（`shasum -a 256`），等於 README 的值才用 `git am` 套用；失敗就 `git am --abort` 並停下。
7. clippy：警告數不比基準多就算通過。
8. 第一次 push 一律 `git push -u origin <分支名>`。
9. 指令一律用 `sh` 或照任務書原樣執行；**不要在 zsh 裡自己寫迴圈拆參數**（上一批 `set -- $row` 在 zsh 不會拆字）。
10. 報告最上面是「步驟對照表」，接著「卡住的地方」，再來「任務書沒寫但我做了的事」。
11. 時間上限 45 分鐘。

## 軌 C

指令：`讀 ~/audit-loupe/docs/lab/task-1021d.md，嚴格照步驟。`

補丁與說明：`~/audit-loupe-lab/patches/trackC/`（先讀 `README.md`）。

- **C-1：前提。**`gh pr view 16 --json state`、`gh pr view 15 --json state` 都要是 `MERGED`。不是就停下來，寫進報告，整個任務結束。
- **C-2：開工作目錄。**
  1. 在 `~/audit-loupe`：`git fetch origin`，`git worktree add ../audit-loupe-read -b perf/read-path-lean origin/main`。
  2. `mv ~/audit-loupe/docs/lab/task-1021d.md ~/audit-loupe-read/docs/lab/task-1021d.md`（把這份任務書搬進新分支）。
  3. 在 `~/audit-loupe-read/backend`：`cargo clippy --all-targets 2>&1 | tee ~/audit-loupe-lab/patches/trackC/clippy-main.txt`，記下主程式、測試的警告數。
- **C-3：套補丁。**比對 `0001-read-path-gzip-server-timing-stats.patch` 的 SHA-256，在 `~/audit-loupe-read` 用 `git am` 套用。
- **C-4：測試。**在 `backend`：
  1. `cargo test --locked`：單元 116 通過、整合 10 通過、0 失敗。不一致就停下來。
  2. `cargo clippy --all-targets`：不比 C-2 多。
  3. `grep -rn "AL_READ_CONNS\|read_pool" src/` 要沒有結果。
- **C-5：帶上兩份量測報告。**分支 `perf/read-path-v2` 上有 4 個只放報告的 commit：`20197b3`、`e65c31f`、`e827e82`、`514eecf`。
  1. 每個都先 `git show --stat <commit>`：只動 `docs/lab/report-perf-read-path.md` 或 `docs/lab/report-perf-concurrency.md` 才可以帶。
  2. 都符合就照這個順序 `git cherry-pick`。有一個不符合就全部不帶，寫進報告。
- **C-6：push、開 PR。**`git add docs/lab/task-1021d.md`，commit（訊息 `lab: 10/21 fourth-batch task book`），`git push -u origin perf/read-path-lean`，開 PR 到 `main`，標題 `read path: gzip, Server-Timing, small tables (no read pool)`，描述貼 README 的「做了什麼」表。
- **C-7：量測。**
  1. 在 `backend`：`cargo build --release --locked`，`rm -f ~/audit-loupe-lab/perf/bin/audit-loupe-lean && cp target/release/audit-loupe ~/audit-loupe-lab/perf/bin/audit-loupe-lean`。
  2. 記下 `uptime`。
  3. 在 `~/audit-loupe-lab/perf/`，用 `sh` **一行一行**執行下面 4 個指令（同時只跑一個；`mainA` 是上一批的執行檔，重跑是為了在同樣的機器負載下比較）：
     ```
     ./concurrency-matrix.sh bin/audit-loupe-mainA pristine-10x.db mainA2-10x-wno - no > results-matrix-mainA2-10x-wno.tsv
     ./concurrency-matrix.sh bin/audit-loupe-lean pristine-10x.db lean-10x-wno - no > results-matrix-lean-10x-wno.tsv
     ./concurrency-matrix.sh bin/audit-loupe-mainA pristine-10x.db mainA2-10x-wyes - yes > results-matrix-mainA2-10x-wyes.tsv
     ./concurrency-matrix.sh bin/audit-loupe-lean pristine-10x.db lean-10x-wyes - yes > results-matrix-lean-10x-wyes.tsv
     ```
  4. 刪掉這 4 組的工作副本：`work-mainA2-*`、`work-lean-*`（含 `-wal`、`-shm`）。只刪這兩種前綴。
- **C-8：報告。**`~/audit-loupe-read/docs/lab/report-read-path-lean.md`：
  1. 表格：4 組各輪的最短–最長秒數、`bytes`（有 `Accept-Encoding: gzip`）、`lean` 的 `Server-Timing` db、`checkpoint_passive`、load 開始／結束。
  2. 只寫數字的比較：`lean` 和 `mainA2` 在同樣條件下差多少。不寫數字以外的推論。
  3. commit、`git push`（不 force），把表格加進 PR 描述。
  4. 等 CI 跑完，結果寫進報告再 push 一次。CI 失敗就寫進報告，不要自己改程式。
