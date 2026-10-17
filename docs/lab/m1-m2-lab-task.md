# Audit Loupe lab 實測任務書（M1＋M2）

給 Sam Mac 上的 CLI agent。寫作時間 2026-10-17，作者是 Cowork 那一側的 Claude（碰不到 lab，所有 lab 相關的事都沒實跑過）。

---

## 0. 取得程式碼

1. 若 `https://github.com/yansheng133/audit-loupe` 已存在且有 `main`：`git clone` 它。
2. 否則用 Sam 放在本機的 bundle 檔（`audit-loupe.bundle`，Sam 會告訴你放在哪個資料夾）：
   `git clone <bundle 檔完整路徑> audit-loupe`
3. 本任務書在 repo 的 `docs/lab/m1-m2-lab-task.md`；其他引用的檔案都在同一個 repo，路徑以 repo 根目錄為準。

---

## 1. 現況

### 1.1 已確認（不用重查）

程式碼（repo `main`，最後一個 commit 見 `git log -1`）：

- 後端是單一 Rust 程式 `audit-loupe`（`backend/`），子命令決定角色：`server`、`collect-file`、`collect-rancher`、`collect-watch`、`collect-neuvector`、`seed-demo`。`cargo test` 72 個測試全過、無警告。
- 收集元件全部**推送**到 server 的 ingest 埠（8081，`POST /ingest/v1/batch`，bearer token 決定叢集）；UI 讀 server 的 API 埠（8080），經 Rancher service proxy。
- Helm chart：`deploy/chart`（一個 chart、values 決定角色）。RKE2 audit policy：`deploy/policy/rke2-audit-policy.yaml`。安裝說明：`docs/deploy.md`。設計與待驗項目：`docs/design.md`（第 8 節是待 lab 驗證清單，本任務書第 7 節逐條對應）。
- UI extension：`ui/pkg/audit-loupe`，三頁：變更地圖（`changes`）、權限覆核（`access`）、收集狀態（`coverage`），加上每座叢集 Dashboard 的一張卡片。用 `@rancher/shell` 3.0.13 建置成功（`ui/` 下 `./node_modules/@rancher/shell/scripts/build-pkg.sh audit-loupe`）。
- 映像檔名稱（**尚未建置、尚未推送**）：`docker.io/yansheng133/audit-loupe-server`、`docker.io/yansheng133/audit-loupe-collector`，同一個 `backend/Dockerfile`（SUSE BCI rust → bci-micro），腳本 `scripts/build-images.sh`。Dockerfile 沒有交叉編譯，**在什麼架構上建就是什麼架構**。

### 1.2 上次知道的 lab 狀態——全部未確認，第一步實測

以下來自 2026-09 的紀錄，Sam 之後說過「lab 已經刪掉了」（可能只刪了 OpenCost 那套，也可能整個環境）：

- Mac：M5 Pro、48 GB，**arm64**；lab 用 Lima 管理的 VM，guest OS 為 SLES 16（SELinux enforcing，RKE2 設 `selinux: true`）。沒有 x86-64 節點。
- Rancher UI 曾在 `https://rancher.192.0.2.3.nip.io/`（Rancher 的 server-url，是**主機名稱**），管理一座 RKE2 下游叢集。Mac 上的 Chrome 連不到 lab 網段，Sam 用 `ssh -L` 把 UI 轉到 `https://localhost:8443`。
- kubeconfig 檔的位置、叢集 ID（`local` 以外的 `c-m-xxxxx`）、節點名稱、Lima VM 名稱：**我都不知道**。這四個是不同的字串，請分別查出來寫進報告。

**第一步**：確認 lab 是否還在、Rancher 與 RKE2 的版本（寫作時我不知道版本；不要套用我在文件裡提到的任何版本號）、有幾座下游、`local` 是不是 RKE2、能不能 ssh 進節點、節點上 `sudo` 要不要密碼。lab 不在的話，**停下來回報**，不要自己重建（重建要 Sam 決定）。

---

## 2. 已經是好的，不要碰

- Rancher 本身的設定：只允許改 `auditLog.*` 這組 Helm 值（第 7 節第 1 項）。其他 Rancher Helm 值、全域設定、驗證設定都不要動。
- RKE2：只允許**新增** audit policy（第 7 節第 3 項）。其他 RKE2 設定、CNI、既有元件不要改。
- 不要動名稱以 `exp-` 開頭的 namespace，也不要動別人的 task brief 檔案。
- local 叢集上只新增 `audit-loupe-system` 這個 namespace 裡的東西（Sam 已同意在 local 放收集元件與 server）。
- 不要讀 `~/Desktop/opencost/rancher-lab-apikey*`。

---

## 3. 我的假設可能錯在哪

每一條都寫了我實際查了什麼、沒查什麼。對不上的請照實回報，不要硬改成符合我的寫法。

1. **Rancher audit log 的欄位**：我讀的是 rancher/rancher `main`（2026-10-16）的原始碼，不是 lab 那個版本。`user.identity`、`user.impersonatedBy` 可能根本不在你的版本裡；程式有退回 `user.extra` 的寫法，但**我沒看過任何一行真實的 Rancher audit log**。先抓 5 行真實紀錄對照 `backend/src/rancher.rs` 的 `Entry`。
2. **Steve 的 URI 形式**：我假設 `/v1/apps.deployments/<ns>/<name>`（複數）。**沒驗證過**。用 UI 改一個 Deployment，看 Rancher audit log 實際的 `requestURI`。
3. **sidecar 名稱與 Pod 標籤**：我假設容器叫 `rancher-audit-log`、Pod 標籤 `app=rancher`、在 `cattle-system`。來源是 chart 原始碼，**沒在活的叢集上看過**。
4. **配對兩份紀錄**：Rancher 時間只到秒，我用「Rancher 時間 − 下游時間在 −2 秒到 +3 秒」配對。實際的時間差沒量過。
5. **collect-file 在 SLES 16 SELinux enforcing 下讀 `/var/lib/rancher/rke2/server/logs/audit.log`**：我**完全沒試過**，很可能被 SELinux 擋（容器讀主機的 var_lib 標籤）。被擋時：**不要關 SELinux、不要改成 privileged**；回報 `ausearch`／`dmesg` 的 AVC 紀錄與你建議的最小做法，讓 Sam 決定。
6. **Pod Security**：collect-file 需要 hostPath，我在說明裡要 `audit-loupe-system` 標 `pod-security.kubernetes.io/enforce=privileged`。RKE2 若以 CIS profile 安裝，PSA 由 admission 設定統一套用，標籤可能無效，要加豁免。沒驗證。
7. **Helm chart 從沒用 helm 跑過**：我那邊下載不到 helm，只用 Go text/template 加自己寫的函式渲染了三種情境並解析 YAML。**先跑 `helm lint` 與 `helm template`**，有錯照實修，修了什麼寫進報告。
8. **UI 只在獨立的 Vite 測試頁面跑過**（mock 掉 `$store`、`$route`），沒有在 Rancher 裡載入過。整頁高度用 `calc(100vh - var(--header-height, 55px) - 40px)` 是猜的。`plugin.addProduct(require('./product'))` 加 `addRoutes` 的寫法照官方舊版 DSL，沒在 Rancher 2.15 上驗。
9. **service proxy 的 RBAC 名稱**：我在 `docs/deploy.md` 的 Role 範例寫 `resourceNames: ["http:audit-loupe:8080", "audit-loupe"]`，實際比對的名稱沒驗。
10. **Fleet**：標記（`objectset.rio.cattle.io/id`、`fleet.cattle.io/managed-by-*`）與 fleet-agent 的身分是從 Fleet `main` 原始碼讀的；Rancher 內建的 Fleet 版本可能沒有 `managed-by-*` 標籤；local 叢集上 fleet-agent 的 namespace 可能是 `cattle-fleet-local-system`，沒驗。

---

## 4. 目標（使用者看得到的結果）

做完之後，Sam 在 Rancher UI 裡應該看得到：

1. 左上選單的全域應用有「Audit Loupe」，裡面三頁都打得開、不用捲整頁就看完變更地圖。
2. 在下游叢集用 Rancher 介面改一個 Deployment 的副本數：變更地圖上出現**一列**（不是兩列），證據欄的身分鏈是「帳號 → Rancher 介面 → 代為執行 → 下游 apiserver」，改了什麼有 diff。
3. 用 Rancher 下載的 kubeconfig 以 kubectl 改同一個物件：經由顯示「Rancher kubectl」。
4. 在下游節點上用 `/etc/rancher/rke2/rke2.yaml`（節點管理者憑證）刪一個測試 namespace：標「繞過 Rancher」、高風險，出現在「需要注意」。
5. 在 Rancher 給一個測試使用者某專案的成員權限：權限覆核頁看得到這個人、這個權限、授權者是誰。
6. 收集狀態頁：每個收集元件都「正常」；停掉一個收集元件 5 分鐘後，那一座叢集顯示中斷。

---

## 5. 建議做法

### A（優先）：在 Mac 上建 arm64 映像檔、推到 Docker Hub

- 先查 Mac 上有沒有 `docker`／`nerdctl`／`podman`（不要裝 Docker Desktop；要裝任何東西先問 Sam）。
- `scripts/build-images.sh 0.2.0`（不加 `--push`）先建。推送需要 Docker Hub 登入：**登入由 Sam 自己做**，你只寫好指令讓他用 `! bash <腳本>` 執行。
- 只有 arm64 映像檔沒關係（lab 全是 arm64）；報告裡寫明。

### B：在 lab VM 裡建，直接匯入節點

Mac 沒有可用的建置工具時：在一台 lab VM 用 `nerdctl build`／`buildah` 建，`ctr -n k8s.io images import` 匯入每個要跑的節點，chart 設 `image.pullPolicy=Never`。

### C（最後手段）：人工

UI extension 經 Helm repo 裝不起來時，改用 Rancher 的「Developer Load」：在 `ui/` 跑 `yarn serve-pkgs`（預設 `localhost:4500`），在 Extensions 頁用 Developer Load 載入 `http://localhost:4500/audit-loupe-0.2.0/audit-loupe-0.2.0.umd.min.js`（實際檔名看 `ui/dist-pkg`）。瀏覽器是經 ssh tunnel 開 Rancher，`localhost:4500` 從同一台 Mac 開得到。

**任何一步卡超過 30 分鐘就停下，寫進報告，不要為了自動化耗掉整個下午。**

---

## 6. 硬性限制

- **每一條 `kubectl`／`helm` 都要明確指定 `--kubeconfig`**（local 與下游各一個檔）。沒指定會打到 `~/.kube/config` 的 current-context。
- Mac 本機不用 sudo、不改 macOS 或 Chrome 設定。lab VM 裡需要 sudo 時照實用；要密碼就停下回報。
- port-forward 一律綁 `127.0.0.1`。
- token、密碼、API key 不寫進任何檔案、不 `echo`、不進 argv。Audit Loupe 的叢集 token 在單一腳本內產生並直接寫進 Secret，例如：

  ```bash
  T=$(openssl rand -hex 32)
  kubectl --kubeconfig "$KC" -n audit-loupe-system create secret generic audit-loupe-collector-token --from-file=token=<(printf %s "$T")
  ```

  server 的 tokens Secret 同理（每行「<叢集 ID> <token>」）。腳本結束前 `unset T`。
- 需要 Rancher 管理者 API token 或 Docker Hub 登入的步驟：寫成腳本，請 Sam 用 `! bash <腳本>` 執行。腳本登入產生的 Rancher session token 結束時要登出。
- 測試使用者：用完刪除，報告列出建立與刪除了哪些帳號。
- 不要給 audit-loupe 任何超出 chart 裡寫的權限。

---

## 7. 驗證

每一項都要回報：做法、實際觀察到的東西（貼原始輸出的關鍵幾行，**遮掉 token**）、結論 yes/no。不接受「看起來正常」。

### 7.1 安裝前

| # | 驗什麼 | 怎麼驗 | 陷阱 |
|---|---|---|---|
| 0 | chart 正確 | `helm lint deploy/chart`；`helm template` 三種 values：local（server＋watch＋rancher）、下游（watch＋file）、全部關閉 | 我只用自製渲染器跑過 |
| 1 | 開 Rancher audit log 時 Pod 會滾動重建、中斷多久 | 改 `auditLog.enabled=true,level=1,destination=sidecar`；同時跑每秒一次的 `curl -sk https://<rancher>/ping`，記錄失敗的秒數 | 用 server-url 的主機名稱，不要用 IP |
| 2 | sidecar 名稱、Pod 標籤、紀錄格式 | `kubectl get pod -n cattle-system -l app=rancher -o jsonpath` 看容器名；`kubectl logs -c rancher-audit-log` 取 5 行 | 對照第 3 段第 1～3 點 |
| 3 | 下游套用 audit policy 的重啟順序與時間 | Rancher 建立的叢集：`machineGlobalConfig` 加 `audit-policy-file`（值是檔案**內容**）；記錄每台節點 rke2-server 重啟時間 | 不要用 `kube-apiserver-arg` 傳 |
| 4 | 不登入節點就看得出 audit 有沒有開 | `kubectl get pod -n kube-system -l component=kube-apiserver -o yaml` 看有沒有 `--audit-policy-file`、`--audit-log-path` | |

### 7.2 裝好之後（第 4 段的六項，加下列）

| # | 驗什麼 | 怎麼驗 | 陷阱 |
|---|---|---|---|
| 5 | exec、port-forward、介面 shell 不進 Rancher audit log | 經 Rancher kubeconfig `kubectl exec`；從 Rancher 介面開 shell。比對 Rancher audit（sidecar 輸出）與下游 audit.log | 用 `kubectl logs` 撈，不要對遠端跑 `grep <字串>`（會匹配到自己的指令） |
| 6 | Rancher Pod 重建後收集元件會接續、不重複 | 刪一個 Rancher Pod（lab，可以）；之後 `/api/timeline?all=1&hours=2&limit=2000`（經 port-forward 到 server 的 8080，綁 127.0.0.1），依每筆的 `audit_id`＋`cluster` 計數，不可有重複 | 只看筆數不夠，要依 auditID 查重複。映像檔裡沒有 `sqlite3` 也沒有 `tar`，不要嘗試在容器裡查資料庫 |
| 7 | 變更地圖四種情境 | 第 4 段 2、3、4 項，加「收集中斷」：把 `audit-loupe-watch` scale 成 0、改一個物件、10 分鐘後 scale 回 1 | 中斷期間的變更應顯示「無法歸屬」；scale 的是我們自己的元件 |
| 8 | 紀錄量 | 套 policy 前後各 10 分鐘的 audit.log 行數與大小；server DB 大小一天的成長 | 前後要在相同負載下量 |
| 9 | 整頁版面 | 1440×900 與 1920×1080 各截一張圖；看三欄各自捲動、時間軸固定在底部 | |
| 10 | Steve URI 形式、`user.identity` 是否存在 | 7.2 第 2 項做完後，從 Rancher audit 取那一筆原始紀錄 | 回報原始 JSON（遮掉 token、cookie） |
| 11 | 刪除使用者後 Rancher 清掉哪些物件 | 建一個本機測試使用者、給專案權限、刪掉；權限覆核的「期間變化」應顯示「刪除使用者後由 Rancher 自動清除」 | |
| 12 | 誰看得到 | 用一個**沒有** `services/proxy` 權限的測試使用者開 Audit Loupe：應看到「找不到後端或沒有權限」；再給第 6 節的 Role，應看得到 | 不要用 `kubectl auth can-i --as` 經 Rancher 代理驗（impersonation 會被吃掉，永遠回你自己的權限）；直接用該使用者登入 UI 看結果 |
| 13 | Fleet 標記與改回 | 在 `fleet-local` 或下游部署一個最小的 GitRepo（例如 rancher/fleet-examples 的 simple）；看物件上的標記；手動改一個欄位，看變更地圖是否標「繞過 GitOps」；打開 `correctDrift` 再改一次，`/api/gitops` 應顯示已被改回 | 記下 local 叢集 fleet-agent 的實際 namespace |
| 14 | 雜湊鏈與 SIEM（選做） | `/api/integrity` 全部叢集 `first_bad_id` 為 null | 不需要真的 SIEM |
| 15 | SUSE Security（只在已安裝時做） | Monitor 模式下 exec 進去跑 `curl`，看證據欄是否列出 | 沒裝就略過，報告寫「未做」 |
| 16 | ACE（只在已開啟時做） | 經 ACE 直連下游改物件，看紀錄有沒有帶 IdP 欄位 | 沒開就略過 |

### 7.3 反向與回歸

- **反向**：Audit Loupe 的任何元件都不能寫入叢集。在節點上用 `/etc/rancher/rke2/rke2.yaml`（直連 apiserver，**不要用 Rancher 產生的 kubeconfig**，那會經 Rancher 代理、`--as` 失效）跑 `kubectl auth can-i --list --as=system:serviceaccount:audit-loupe-system:audit-loupe-watch`，只能有 get/list/watch。再用同一個 `--as` 問一題它不該有的（`kubectl auth can-i create deployments -A`），必須回 no；兩題都回 yes 表示 `--as` 沒生效。
- **反向**：ConfigMap 的值不能出現在 server 的資料庫。建一個含 `PASSWORD=lab-canary-123` 的 ConfigMap，等 1 分鐘，先確認 `/api/history` 查得到這個 ConfigMap（證明它確實被收進來了，否則下一步的 0 筆沒有意義），再到 server PVC 所在的節點，對 PVC 目錄裡的 `audit-loupe.db` 與 `audit-loupe.db-wal` 跑 `grep -c lab-canary-123`，兩個都應為 0。
- **回歸**：Rancher UI 正常、下游叢集 Active、裝之前就在跑的工作負載 Ready 數不變（裝之前先記一份）。

### 7.4 真實紀錄樣本（給客戶簡報用）

把第 4 段第 2 項那次操作的 Rancher audit 原始一行、下游 audit 原始一行存成檔案（`docs/lab/samples/`），遮掉 token、cookie、IP 以外的識別資訊，並在旁邊附一行說明各欄位意義。不要放客戶名稱。

---

## 8. 回報格式

1. lab 現況（第 1.2 段逐項：實際值）
2. 我錯在哪（第 3 段逐條：對／錯／沒驗，錯的話實際是什麼）
3. 採用哪條路（A／B／C）與原因
4. 第 7 段逐格矩陣：每一項 yes／no／未做，附證據
5. **任務書沒提到、你自己做的修改**（含改了哪些程式或 chart，請開成 commit，訊息寫清楚原因）
6. 沒做完的部分卡在哪
7. **這份任務書哪裡害你走冤枉路**
8. 收尾：建立過的測試帳號、臨時權限、port-forward、暫存檔，已清除與未清除的清單

報告裡不可以出現任何 token、密碼、API key。
