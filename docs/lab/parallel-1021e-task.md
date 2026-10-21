# 任務書：10/21 第五批——0.4.0 預覽版準備（軌 R）、第 1 層批次 1（軌 A、軌 D）

寫作：2026-10-21 晚。Sam 已同意：先發 0.4.0 預覽版，再做第 1 層，之後發第一個正式版。計畫：`~/audit-loupe-lab/plans/layer1-plan-v2.md`。三軌平行。

## 0. 照任務書執行（違反就是不合格）

1. **只做本任務書列出的步驟，照編號順序做。**每一步做完，在報告的「步驟對照表」填：步驟編號、做了什麼、證據。
2. **任務書沒寫的動作一律不做**，包括但不限於：
   - 安裝常駐服務、排程或背景程式（包括「暫時的」背景監看）。
   - 改程式、改補丁內容。
   - 改 CI、lint、建置、相依套件的設定。**例外：補丁本身改到的檔案**（各補丁 README 有列），套用補丁不算違反。
   - 刪除檔案或分支（步驟有寫的除外；自己產生的錯檔也不要刪，寫進報告）。
   - force-push、合併 PR、打 tag、發 release、改寫歷史。
   - 改 lab：helm upgrade／rollback、apply、patch、delete、重啟 Pod。**這一批對 lab 全程只讀。**
3. **照做行不通、或任務書沒寫到的情況**：停在那一步，讓程式維持可編譯、測試會過的狀態，在報告的「卡住的地方」寫清楚，跳到下一個不依賴它的步驟。**不要自己換做法、不要擴大範圍。**
4. **發現任務書寫錯**：照字面做會造成損害就停下來；不會就照做。兩種情況都要在報告指出。
5. 硬性限制照舊：
   - 不印也不存 token、密碼、IP。不用會印出 IP 的指令（`lsof -i`、`ifconfig`、`kubectl get … -o wide`）。
   - Mac 不用 sudo。
   - 不開 kubeconfig、`env.sh`、`~/audit-loupe-lab/perf/tokens`、`~/audit-loupe-lab/v-*.yaml` 的內容。
   - 每條 `kubectl`／`helm` 都寫 `--kubeconfig`。port-forward 只綁 127.0.0.1。
   - 原始資料唯讀：`~/audit-loupe-lab/overnight/`、`recordings/`、`perf/pristine-*.db`、`proto/db/`。
   - 本機服務只綁 127.0.0.1；**不碰 18090–18097 與它們的資料庫**。
   - CLI 不合併 PR。
6. 補丁先比對 SHA-256（`shasum -a 256`），等於 README 的值才用 `git am` 套用；失敗就 `git am --abort` 並停下。
7. clippy：警告數不比基準多就算通過（基準在各軌先量）。
8. 第一次 push 一律 `git push -u origin <分支名>`。
9. 指令照原樣執行；**不要在 zsh 裡自己寫迴圈**。要重複的指令就一行一行寫出來執行。
10. 報告最上面是「步驟對照表」，接著「卡住的地方」，再來「任務書沒寫但我做了的事」（應該是空的）。
11. 時間上限：軌 R 60 分鐘、軌 A 60 分鐘、軌 D 75 分鐘。

## 軌 R：0.4.0 預覽版準備（只準備，不打 tag、不改 lab）

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021e-task.md，做「軌 R」，嚴格照步驟。`

補丁：`~/audit-loupe-lab/patches/release-0.4.0/`（先讀 `README.md`）。

- **R-1：收任務書。**在 `~/audit-loupe`：
  1. `git status`：應該只有兩個新檔，這份任務書和第六批的 `docs/lab/task-1022a.md`。有別的變更不要動，寫進報告。
  2. `git fetch origin`，`git switch -c docs/task-1021e origin/main`。
  3. 只 commit 這兩份任務書（`git add docs/lab/parallel-1021e-task.md docs/lab/task-1022a.md`），`git push -u origin docs/task-1021e`，開 PR 到 `main`，標題 `docs: 10/21 fifth- and sixth-batch task books`。第六批這次不執行，只是一起收進 repo。
- **R-2：後端發版 PR。**
  1. `git worktree add ../audit-loupe-rel -b release/0.4.0 bc52503`（`git rev-parse HEAD` 要以 `bc52503` 開頭）。
  2. 比對 `backend-0001-release-0.4.0.patch` 的 SHA-256，在 `~/audit-loupe-rel` 用 `git am` 套用。
  3. 在 `backend`：`cargo test --locked`，單元 116、整合 10 通過、0 失敗。
  4. `git push -u origin release/0.4.0`，開 PR 到 `main`，標題 `release: 0.4.0 (preview)`。描述貼 `CHANGELOG.md` 的 0.4.0 一節，最後一行寫：「合併後由下一份任務書在這個合併 commit 打 tag `v0.4.0`。」
- **R-3：介面發版 PR。**
  1. `git -C ~/audit-loupe-ui worktree add ../audit-loupe-ui-rel -b release/0.4.0 8cb95bef620bec2c106a9b38d901f2c32d8062fa`。
  2. 比對 `ui-0001-release-0.4.0.patch` 的 SHA-256，在 `~/audit-loupe-ui-rel` 用 `git am` 套用。
  3. `yarn lint` 與 `yarn build-pkg audit-loupe` 都要通過（用既有的指令與容器做法，不改設定）。
  4. `git push -u origin release/0.4.0`，開 PR 到 `main`，標題 `release: 0.4.0 (preview)`，描述貼 `CHANGELOG.md` 的 0.4.0 一節。
- **R-4：lab 升級前存證（只讀）。**全部存到 `~/audit-loupe-lab/release-0.4.0/`（新資料夾，不進 repo；建好後 `chmod 700`）。兩座叢集：local 用 `~/audit-loupe-lab/local.kubeconfig`，下游用 `~/audit-loupe-lab/ds-admin.kubeconfig`。
  1. 每座：`helm --kubeconfig <檔> -n audit-loupe-system history audit-loupe` 存成 `helm-history-<local|ds>.txt`。
  2. 每座：`helm --kubeconfig <檔> -n audit-loupe-system get values audit-loupe -o yaml > values-<local|ds>.yaml`，接著 `chmod 600`。**不要印出或打開這兩個檔**（可能有 token）。
  3. 每座：`kubectl --kubeconfig <檔> -n audit-loupe-system get pods` 存成 `pods-<local|ds>.txt`；映像檔：`kubectl --kubeconfig <檔> -n audit-loupe-system get pods -o jsonpath='{range .items[*]}{.metadata.name}{" "}{.spec.containers[*].image}{"\n"}{end}'` 存成 `images-<local|ds>.txt`。
  4. 只有 local：`kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system port-forward --address 127.0.0.1 svc/audit-loupe 18180:8080`（做完這一步就停掉）。存下 `curl -s 127.0.0.1:18180/api/coverage?hours=48`、`curl -s 127.0.0.1:18180/api/integrity`、`curl -s 127.0.0.1:18180/api/info`、`curl -s 127.0.0.1:18180/api/rules` 成 `coverage.json`、`integrity.json`、`info.json`、`rules.json`。
  5. 只有 local：server 資料庫照上一次的做法（`docs/lab/report-ui-facts.md` 開頭）：`kubectl exec <server Pod> -- cat /data/audit-loupe.db > db/audit-loupe.db`，`-wal` 同樣做；`chmod 600`；再 `sqlite3 -readonly db/audit-loupe.db 'PRAGMA quick_check'` 要是 `ok`，並記下 `SELECT count(*) FROM events`。
  6. 寫好**還原程序**放進報告（下一份任務書才會用到，這次不執行）：兩座各自 `helm rollback` 到第 1 步記下的 revision、確認映像檔回到 `images-*.txt`；資料庫只有在新版開不起來時才需要放回，具體做法列出來，標明「需要 Sam 同意才執行」。
- **R-5：報告。**`~/audit-loupe-rel/docs/lab/report-release-0.4.0-prep.md`，commit 在 `release/0.4.0`，`git push`（不 force）。等兩個 PR 的 CI 跑完，結果寫進報告再 push 一次。

## 軌 A：身分表與白名單（第 1 層 PR-A）

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021e-task.md，做「軌 A」，嚴格照步驟。`

補丁：`~/audit-loupe-lab/patches/l1a/`（先讀 `README.md`）。

- **A-1：開工作目錄、量基準。**
  1. 在 `~/audit-loupe`：`git fetch origin`，`git worktree add ../audit-loupe-id -b feat/identities bc52503`。
  2. 在 `backend`：`cargo clippy --all-targets`，記下主程式、測試的警告數。
- **A-2：套補丁。**比對 `0001-identities-whitelist.patch` 的 SHA-256，用 `git am` 套用。
- **A-3：測試、push、開 PR。**
  1. `cargo test --locked`：單元 122、整合 10 通過、0 失敗。
  2. `cargo clippy --all-targets`：不比 A-1 多。
  3. `git push -u origin feat/identities`，開 PR 到 `main`，標題 `identities: classify once, with a whitelist; rules ask who is a person`，描述貼 README 的「做了什麼」表，第一行寫「第 1 層 PR-A；請在 0.4.0 發版 PR 之後合併」。
- **A-4：用真實資料預覽升級的影響。**全部放在 `~/audit-loupe-lab/release-0.4.0/id-preview/`（新資料夾）。
  1. `cargo build --release --locked`。
  2. `sqlite3 -readonly ~/audit-loupe-lab/proto/db/al-copy.db ".backup $HOME/audit-loupe-lab/release-0.4.0/id-preview/id.db"`。
  3. 升級前的警示數：`sqlite3 -readonly id.db "SELECT rule, severity, count(*) FROM alerts GROUP BY 1, 2 ORDER BY 1, 2"`。
  4. `touch empty-tokens`，然後在前景以外啟動（`nohup … &`，記下 PID）：
     `AL_DB_PATH=$HOME/audit-loupe-lab/release-0.4.0/id-preview/id.db AL_LISTEN=127.0.0.1:18098 AL_INGEST_LISTEN=127.0.0.1:18099 AL_CLUSTER=local AL_RANCHER_CLUSTER=local AL_TZ=Asia/Taipei AL_RETENTION_DAYS=3650 AL_INGEST_TOKENS_FILE=$HOME/audit-loupe-lab/release-0.4.0/id-preview/empty-tokens $HOME/audit-loupe-id/backend/target/release/audit-loupe server`
  5. `curl -s 127.0.0.1:18098/api/identities`：用 `python3 -c` 整理成「分類 × 依據 的身分數」，並列出「名稱不是 `system:` 開頭、但分類不是人」的身分（只列名稱、分類、說明）。
  6. `curl -s 127.0.0.1:18098/api/rules`：記下 `pending`（有幾筆要重新判斷）。
  7. `curl -s -X POST 127.0.0.1:18098/api/rules/recompute`，然後每 10 秒 `curl -s 127.0.0.1:18098/api/rules` 一次（一行一行執行，最多 30 次），直到 `job` 與 `pending` 都是 null；記下耗時。
  8. 用第 4 步的 PID 停掉 server，再做一次第 3 步的查詢。
  9. 報告列出：升級前後各規則的警示數與差值；哪些身分造成差異（從第 5 步對得上的就寫）。
- **A-5：報告。**`~/audit-loupe-id/docs/lab/report-identities.md`，commit、`git push`（不 force）。等 CI 跑完，結果寫進報告再 push 一次。`id-preview/` 留著，不刪。

## 軌 D：效能閘門（第 1 層 PR-D）

指令：`讀 ~/audit-loupe/docs/lab/parallel-1021e-task.md，做「軌 D」，嚴格照步驟。`

補丁：`~/audit-loupe-lab/patches/l1d/`（先讀 `README.md`）。

- **D-1：開工作目錄、量基準。**
  1. 在 `~/audit-loupe`：`git fetch origin`，`git worktree add ../audit-loupe-gate -b feat/perf-gate bc52503`。
  2. 在 `backend`：`cargo clippy --all-targets`，記下警告數。
- **D-2：套補丁。**比對 `0001-perf-gate.patch` 的 SHA-256，用 `git am` 套用。
- **D-3：測試、push、開 PR。**
  1. `cargo test --locked`：單元 117 通過（另有 3 個略過）、整合 10 通過、0 失敗。
  2. `cargo clippy --all-targets`：不比 D-1 多。
  3. `git push -u origin feat/perf-gate`，開 PR 到 `main`，標題 `perf gate: the change map at about 720,000 records a day, recorded in CI`，描述貼 README 的「做了什麼」表，第一行寫「第 1 層 PR-D；請在 0.4.0 發版 PR 之後合併」。
- **D-4：在 Mac 跑一次。**記下開始前的 `uptime`，然後在 `backend` 執行 `cargo test --release --locked perf_gate -- --ignored --nocapture`，把印出的表格原樣放進報告，並寫明當時有哪些其他軌在編譯（`pgrep -fl 'cargo|rustc'` 的結果，只列行程名稱）。
- **D-5：CI 的結果。**等 PR 的 CI 跑完：
  1. `perf (record only)` 這個 job 有沒有成功、花多久。
  2. 用 `gh run view <run-id> --log --job <job-id>` 找出表格，原樣放進報告（找不到就寫找不到，不要重跑）。
  3. `backend tests`、`helm chart` 的結果。
- **D-6：報告。**`~/audit-loupe-gate/docs/lab/report-perf-gate.md`，commit、`git push`（不 force）。

## 三軌之間

- **開工順序**：先開軌 R；軌 R 回報 R-1 完成（任務書 PR 開好）之後，再開軌 A，隔一分鐘再開軌 D。三軌都在同一個 repo 做 `git fetch`／`git worktree add`，同時做會互搶 git 的鎖。
- 三軌互不碰對方的工作目錄。軌 R 是唯一碰 lab 的軌，而且只讀。
- 軌 A 用 18098／18099；軌 D 不起 server；軌 R 只用 18180 的 port-forward。
- **合併順序（給 Sam）**：先合併 0.4.0 發版 PR，下一份任務書在它的合併 commit 打 tag；第 1 層的 PR-A、PR-D 在那之後再合併。
