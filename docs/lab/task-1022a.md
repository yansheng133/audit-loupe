# 任務書：10/22 第六批——本機預覽換成第 1 層（軌 P）、第 1 層 PR（軌 Q）、0.4.0 上線（軌 R2）

寫作：2026-10-21 夜。補丁與說明：`~/audit-loupe-lab/patches/l1f/`（先讀 `README.md`）。

- **軌 P** 不必等任何 PR 合併：把 Sam 看的本機 server（18090）換成第 1 層的程式，介面修正開 PR。**要等第五批的軌 A、軌 D 都做完才開**：軌 A 的 A-4 要從 18090 的 `al-copy.db` 複製升級前的資料，軌 D 的 D-4 在 Mac 量效能，軌 P 換執行檔與編譯會影響兩者。
- **軌 Q** 要等第五批的三個 PR（0.4.0 發版、PR-A、PR-D）都合併。
- **軌 R2** 會改 lab。**只有 Sam 下指令時明確寫「含 R2」才做**，否則整軌跳過。

軌 P、軌 Q 可以平行；軌 R2 在軌 P 之後做。

## 0. 照任務書執行（違反就是不合格）

1. **只做本任務書列出的步驟，照編號順序做。**每一步做完，在報告的「步驟對照表」填：步驟編號、做了什麼、證據。
2. **任務書沒寫的動作一律不做**，包括但不限於：
   - 安裝常駐服務、排程或背景程式（包括「暫時的」背景監看）。
   - 改程式、改補丁內容。
   - 改 CI、lint、建置、相依套件的設定。**例外：補丁本身改到的檔案**（README 有列），套用補丁不算違反。
   - 刪除檔案或分支（步驟有寫的除外；自己產生的錯檔也不要刪，寫進報告）。
   - force-push、合併 PR、改寫歷史。打 tag、發 release 只在軌 R2 寫到的步驟做。
   - 改 lab（helm upgrade／rollback、apply、patch、delete、重啟 Pod）：只在軌 R2 寫到的步驟做。
3. **照做行不通、或任務書沒寫到的情況**：停在那一步，讓程式維持可編譯、測試會過的狀態，在報告的「卡住的地方」寫清楚，跳到下一個不依賴它的步驟。**不要自己換做法、不要擴大範圍。**
4. **發現任務書寫錯**：照字面做會造成損害就停下來；不會就照做。兩種情況都要在報告指出。
5. 硬性限制照舊：
   - 不印也不存 token、密碼、IP。不用會印出 IP 的指令（`lsof -i`、`ifconfig`、`kubectl get … -o wide`）。
   - Mac 不用 sudo。
   - 不開 kubeconfig、`env.sh`、`~/audit-loupe-lab/perf/tokens`、`~/audit-loupe-lab/v-*.yaml` 的內容；`release-0.4.0/values-*.yaml` 也不要打開或印出。
   - 每條 `kubectl`／`helm` 都寫 `--kubeconfig`。port-forward 只綁 127.0.0.1。helm 不加 `--wait`（helm v4 會卡住），改用 `kubectl rollout status`。
   - 原始資料唯讀：`~/audit-loupe-lab/overnight/`、`recordings/`、`perf/pristine-*.db`、`proto/db/`（18090 開著的 `al-copy.db` 只能由新版 server 自己更新結構，不能用其他方式改）。
   - 本機服務只綁 127.0.0.1。18090 只有軌 P 的 P-4 可以動，而且只能用 `deploy-bin.sh`；18091–18097 都不碰。
   - CLI 不合併 PR。
6. 補丁先比對 SHA-256（`shasum -a 256`），等於 README 的值才套用；`git am` 失敗就 `git am --abort` 並停下。
7. clippy：警告數不比基準多就算通過（主程式 17、測試 19）。
8. 第一次 push 一律 `git push -u origin <分支名>`。
9. 指令照原樣執行；**不要在 zsh 裡自己寫迴圈**。要重複的指令就一行一行寫出來執行。
10. 報告最上面是「步驟對照表」，接著「卡住的地方」，再來「任務書沒寫但我做了的事」（應該是空的）。報告放 `~/audit-loupe-lab/reports/report-1022a.md`（`reports/` 不存在就建立），不進 repo。
11. 時間上限：軌 P 60 分鐘、軌 Q 45 分鐘、軌 R2 90 分鐘。

## 軌 P：本機預覽換成第 1 層、介面修正開 PR

- **P-1：前提。**
  1. `cd ~/audit-loupe-lab/patches/l1f && shasum -a 256 backend/*.patch ui/*.patch`，18 個值都要等於 README。
  2. `shasum -a 256 ~/audit-loupe-lab/patches/l1a/0001-identities-whitelist.patch ~/audit-loupe-lab/patches/l1d/0001-perf-gate.patch`，要等於各自 README 的值。
  3. `mkdir -p ~/audit-loupe-lab/reports`
  4. `curl -s -m 5 127.0.0.1:18090/healthz` 要回 `ok`；`curl -s -m 5 127.0.0.1:18090/api/info > ~/audit-loupe-lab/reports/p1-info-before.json`。
- **P-2：開預覽用的工作目錄**（只在本機，不 push）。
  1. `cd ~/audit-loupe && git fetch origin`
  2. `git worktree add ../audit-loupe-l1f -b preview/l1f bc52503`
  3. `cd ~/audit-loupe-l1f`
  4. `git am -3 ~/audit-loupe-lab/patches/l1a/0001-identities-whitelist.patch`
  5. `git am -3 ~/audit-loupe-lab/patches/l1d/0001-perf-gate.patch`
  6. `git am ~/audit-loupe-lab/patches/l1f/backend/*.patch`
  7. `git log --oneline -10` 放進報告：最上面 9 個是 l1f 的補丁，接著 perf gate、identities、`bc52503`。
- **P-3：測試。**在 `~/audit-loupe-l1f/backend`：
  1. `cargo test --locked`：單元 131 通過（3 個略過）、整合 10 通過、0 失敗。不一致就停下，P-4 不做。
  2. `cargo clippy --all-targets 2>&1 | grep 'generated'`：主程式 17、測試 19，不能更多。
- **P-4：換掉 18090 的執行檔。**
  1. `cargo build --release --locked`
  2. `~/audit-loupe-lab/proto/deploy-bin.sh ~/audit-loupe-l1f/backend/target/release/audit-loupe`，要印出 `both up`。沒有就停下，把 `tail -30 ~/audit-loupe-lab/proto/logs/server.log` 放進報告（先確認裡面沒有 token；127.0.0.1 以外的位址要遮掉）。
  3. `grep -i 'index' ~/audit-loupe-lab/proto/logs/server.log | tail -5`
- **P-5：確認新版在 18090 上的行為。**每行照原樣執行，輸出放進報告：
  1. `curl -s '127.0.0.1:18090/api/changemap?day=2026-10-20' -o ~/audit-loupe-lab/reports/p5-day.json`
  2. `curl -s '127.0.0.1:18090/api/changemap?day=2026-10-20&from=1792476000000&to=1792483200000' -o ~/audit-loupe-lab/reports/p5-range.json`
  3. `python3 -c "import json;a=json.load(open('$HOME/audit-loupe-lab/reports/p5-day.json'));b=json.load(open('$HOME/audit-loupe-lab/reports/p5-range.json'));print('day',a['counts'],'range',b['counts']);print('timeline same:',a['timeline']==b['timeline']);print('codes:',sorted({r.get('stateReasonCode','MISSING') for r in a['rows']}));print('hidden',a['hidden']);print('meta',a.get('meta',{}).get('progress'))"`
     - 要看到：range 的 `objects`、`ops` 比 day 小；`timeline same: True`；codes 裡沒有 `MISSING`。
  4. `curl -s '127.0.0.1:18090/api/changemap?day=2026-10-19&tab=events' | python3 -c "import json,sys,collections;v=json.load(sys.stdin);print(collections.Counter((e.get('source'),e.get('reasonCode')) for e in v['events']));print(collections.Counter(g['kind'] for g in v['timeline']['gaps']))"`
  5. `curl -s -o /dev/null -w '%{http_code}\n' 127.0.0.1:18090/api/identities`：要是 200。
  6. `curl -s -o /dev/null -w '%{http_code} %{time_total}\n' 'http://127.0.0.1:18090/api/changemap?day=2026-10-20'` 連續執行 5 次（一行一行執行），記下每次的時間。
- **P-6：介面修正開 PR。**
  1. `cd ~/audit-loupe-ui && git fetch origin`
  2. `git worktree add ../audit-loupe-ui-fix -b fix/v5-layout origin/main`
  3. `cd ~/audit-loupe-ui-fix && git am ~/audit-loupe-lab/patches/l1f/ui/*.patch`
  4. `git diff --stat origin/main | tail -1` 要是 `14 files changed, 828 insertions(+), 83 deletions(-)`。
  5. `git push -u origin fix/v5-layout`，開 PR 到 `main`，標題 `v5: layout, language, timeline range, rules page identities`。描述：第一行寫「和第 1 層後端一起用；接 0.4.0 後端也能用（理由顯示後端原文，範圍只調暗）。合併順序由 Sam 決定。」接著貼 README「介面」的表。
  6. 等 PR 的 CI 跑完，結果寫進報告。失敗就寫進報告，不要自己改。
  7. **不要碰 `~/audit-loupe-ui-v5`**（Sam 的 dev server 正在用，內容已經和這 8 個補丁相同）。
- **P-7：報告。**寫好 `~/audit-loupe-lab/reports/report-1022a.md` 的軌 P 部分。工作目錄 `~/audit-loupe-l1f` 留著，不要刪。

## 軌 Q：第 1 層 PR（批次 2）

- **Q-1：前提。**`gh pr list --repo yansheng133/audit-loupe --state merged --limit 20 --json number,headRefName,mergeCommit`，`release/0.4.0`、`feat/identities`、`feat/perf-gate` 三個都要在裡面。少任何一個就寫進報告，整軌結束。
- **Q-2：開分支。**
  1. `cd ~/audit-loupe && git fetch origin`
  2. `git worktree add ../audit-loupe-l1q -b feat/changemap-l1 origin/main`
  3. `cd ~/audit-loupe-l1q && git am ~/audit-loupe-lab/patches/l1f/backend/*.patch`（SHA-256 在 P-1 比對過；軌 P 沒做的話先照 P-1 第 1 步比對）
- **Q-3：測試。**在 `backend`：`cargo test --locked`（單元 131、整合 10、0 失敗）、`cargo clippy --all-targets 2>&1 | grep 'generated'`（17／19）。不一致就停下。
- **Q-4：PR。**`git push -u origin feat/changemap-l1`，開 PR 到 `main`，標題 `change map: layer 1 page (index, reason codes, time range, request-only, collector gaps)`。描述貼 README「後端」的表與「雲端效能閘門」那段。等 CI 跑完（含 `perf (720k records a day)`），結果寫進報告；perf 的表格原樣貼上。

## 軌 R2：0.4.0 上線（只在 Sam 明確寫「含 R2」時做）

- **R2-1：前提。**
  1. `gh pr list --repo yansheng133/audit-loupe --state merged --head release/0.4.0 --json number,mergeCommit`，要有一筆。記下 `mergeCommit.oid`（以下稱 M）。
  2. `~/audit-loupe-lab/release-0.4.0/` 要有第五批 R-4 留下的 `values-local.yaml`、`values-ds.yaml`、`helm-history-*.txt`、`images-*.txt`。少了就停下。
  3. `git -C ~/audit-loupe fetch origin --tags`，`git -C ~/audit-loupe tag -l 'v0.4.0'` 要是空的。已經有就停下。
- **R2-2：打 tag。**
  1. `git -C ~/audit-loupe tag -a v0.4.0 M -m "0.4.0 (preview)"`（M 換成 R2-1 記下的值）
  2. `git -C ~/audit-loupe push origin v0.4.0`
  3. `gh run list --repo yansheng133/audit-loupe --workflow release-images.yml --limit 1`，等它跑完（`gh run watch <id> --repo yansheng133/audit-loupe`）。**失敗就停下**：把失敗的 job 名稱與最後 20 行寫進報告（常見原因是 Docker Hub secrets 沒設，那由 Sam 設，CLI 不碰）。R2-3 以後都不做。
- **R2-3：確認映像檔。**`curl -s -o /dev/null -w '%{http_code}\n' https://hub.docker.com/v2/repositories/yansheng133/audit-loupe-server/tags/0.4.0`、`…/audit-loupe-collector/tags/0.4.0`，兩個都要 200。
- **R2-4：映像檔設定。**每座一行（只印 `image` 這一段，不印其他 values）：
  - `helm --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system get values audit-loupe -o json | python3 -c "import json,sys;print(json.load(sys.stdin).get('image'))"`
  - `helm --kubeconfig ~/audit-loupe-lab/ds-admin.kubeconfig -n audit-loupe-system get values audit-loupe -o json | python3 -c "import json,sys;print(json.load(sys.stdin).get('image'))"`
- **R2-5：升級 local。**
  1. `git -C ~/audit-loupe worktree add ../audit-loupe-v040 v0.4.0`
  2. `helm --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system upgrade audit-loupe ~/audit-loupe-v040/deploy/chart -f ~/audit-loupe-lab/release-0.4.0/values-local.yaml --set image.server.tag=0.4.0 --set image.collector.tag=0.4.0`
  3. `kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system rollout status deploy/audit-loupe --timeout=5m`
  4. `kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system get daemonset -o name`，對列出的每個 daemonset 一行一行執行 `kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system rollout status <名稱> --timeout=5m`。
  5. 任何一步失敗或逾時：**停下，不要 rollback**，把 `kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system get pods` 與失敗的 Pod 的 `describe` 最後 20 行寫進報告，等 Sam 決定（還原程序在第五批 R-5 的報告）。
- **R2-6：升級下游。**同 R2-5 第 2–5 步，kubeconfig 換成 `~/audit-loupe-lab/ds-admin.kubeconfig`，values 換成 `values-ds.yaml`（下游沒有 server，第 3 步略過）。
- **R2-7：確認。**
  1. 每座：映像檔清單照第五批 R-4 第 3 步的指令存成 `~/audit-loupe-lab/release-0.4.0/images-<local|ds>-after.txt`，全部要是 `0.4.0`。
  2. local：`kubectl --kubeconfig ~/audit-loupe-lab/local.kubeconfig -n audit-loupe-system port-forward --address 127.0.0.1 svc/audit-loupe 18180:8080`（做完就停掉）。存 `api/info`、`api/integrity`、`api/coverage?hours=2` 成 `info-after.json`、`integrity-after.json`、`coverage-after.json`；`curl -s -o /dev/null -w '%{http_code} %{time_total}\n' '127.0.0.1:18180/api/changemap'` 要 200。
  3. 報告寫：版本、雜湊鏈結果、收集元件重新啟動留下的「沒有收集」（預期會有，寫起訖）。
- **R2-8：介面 0.4.0 發佈。**前提兩個都要成立，否則這一步跳過並寫進報告：
  1. audit-loupe-ui 的 0.4.0 發版 PR 已合併（`gh pr list --repo yansheng133/audit-loupe-ui --state merged --head release/0.4.0 --json number,mergeCommit`）。
  2. 介面修正 PR（`fix/v5-layout`）**已在它之前合併**（同樣用 `--head fix/v5-layout` 查）。沒合併就不發佈，免得把 `[object Object]` 那一版發出去。
  - 成立時：`gh release create audit-loupe-0.4.0 --repo yansheng133/audit-loupe-ui --target <發版 PR 的 mergeCommit> --title "audit-loupe 0.4.0 (preview)" --notes-file <把 CHANGELOG.md 的 0.4.0 一節存成的檔案>`，然後等 `build-extension-charts` 跑完，結果寫進報告。
- **R2-9：給 Sam 的確認清單**（寫進報告，CLI 不做）：Rancher 的「擴充功能」把 Audit Loupe 更新到 0.4.0；打開變更地圖，看總覽、時間列、右側細節；「收集狀態」頁看重新啟動留下的缺口。

## 給 Sam

- **合併順序**：第五批（0.4.0 發版 → PR-A、PR-D）→ 軌 Q 的 PR。介面修正 PR 在介面 0.4.0 發佈前合併（Sam 10/21 決定）。
- **R2 會改 lab**：要做就在指令裡寫「含 R2」。
