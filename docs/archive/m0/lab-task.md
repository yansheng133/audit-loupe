# 任務書：在 home lab 實際抓 audit log，並部署 audit-loupe 試吃版

給 Sam Mac 上的 Claude Code CLI。寫作日 2026-10-14。
原始碼在本資料夾的上一層（`audit-loupe/`，下稱 repo）。本任務書引用的檔案都在 repo 內，路徑以 repo 根目錄為準。

---

## 1. 現況（已確認，不用重查）

- repo 內容（雲端已建置與測試）：
  - `backend/`：Rust 後端，執行檔名 `audit-loupe`。`cargo test` 21 條全過。
    - 兩個 port：8080 是給 UI 的讀取 API，8081 是 audit webhook 接收端。
    - 雲端只用合成資料測過，**沒有接過真的叢集**（見第 3 段）。
  - `ui/pkg/audit-loupe/`：Rancher UI extension（@rancher/shell 3.0.13）。
    - `build-pkg` 在雲端成功產出 `ui/dist-pkg/audit-loupe-0.1.0/`。
    - 頁面只在雲端用假的外殼（`preview/`）配 demo 資料看過，**沒有載入真的 Rancher**。
  - `deploy/chart/`：Helm chart。**沒跑過 `helm lint`／`helm template`**，雲端沒有 helm。
  - `deploy/rke2/audit-policy.yaml`、`deploy/rke2/webhook-kubeconfig.yaml`、`deploy/rke2/README.md`：未實跑。
  - 雲端另有一位獨立審查者看過程式與本任務書，指出的問題已修正，修正後同樣未實跑。
- home lab 環境：
  - VM：M5 Mac 上的 Lima VM，作業系統 SLES 16，全部是 ARM64。沒有 x86-64 節點。
  - Rancher UI：https://rancher.192.0.2.3.nip.io/ ，從 Mac 經 `ssh -L` 通道以 localhost:8443 存取。
  - Rancher 管理一座下游 RKE2 叢集，同一套環境上跑著 OpenCost demo。
  - 以上是 Sam 先前的描述，叢集 ID、kubeconfig 路徑、節點名稱我手上都沒有，**第一步實測並寫進報告**。
- Sam 已同意：
  - 在 lab 開啟 Rancher 的 API audit log（helm upgrade Rancher）。
  - 在下游叢集加上 audit policy 與 audit webhook。這兩件會修改既有元件，是本任務的例外，其他元件一律不動（見第 6 段）。

## 2. 已經是好的，不要碰

- OpenCost demo 與它的 ingress、cost-report。整個任務過程中與結束後都要能正常開啟（第 7 段有回歸檢查）。
- `exp-*` namespace，以及其他任務書建立的檔案。
- Rancher local cluster：只允許 A 段的 helm upgrade（只改 auditLog 值），**不在 local cluster 安裝任何東西**。
- `~/Desktop/opencost/rancher-lab-apikey*`：不要讀。

## 3. 我的假設可能錯在哪

1. **Rancher 代理在下游 audit log 裡的樣子**是我依官方文件推的，沒看過實物：
   - kubectl 經 Rancher：`user.username` 以 `system:serviceaccount:cattle-impersonation-system:` 開頭。官方文件有寫，但我沒看過實物。
   - Rancher UI 經 Steve：可能改由 `system:serviceaccount:cattle-system:cattle` 代理。這只是審查者的推測，**沒有依據**。
   - `impersonatedUser.extra.username` 是 Rancher 使用者名稱，只有一篇論壇文章提到。
   - 後端 `src/audit.rs` 的 `to_record()` 靠這些判斷「經由 Rancher」。
   - UI 動作與 kubectl 動作**各抓一筆**真的事件比對。不一樣就改 `to_record()` 並補測試，不要改資料去配合程式。
2. **Rancher UI 調整 replicas 送的是哪個請求**：
   - 審查者讀 shell 原始碼後認為，用 +／− 按鈕調整時，每按一次就送一個 PUT。
   - 所以第 4 段要求 Sam 用 Edit Config 直接把 replicas 改成 3，只送一次。
   - 報告寫出實際的 verb 與 subresource。
3. **kube-apiserver 能不能連到 ClusterIP**：
   - 我假設節點上的 kube-apiserver（host network）連得到 Service 的 ClusterIP（chart 預設 10.43.0.200）。
   - 這只是假設，沒測過。先在 control plane 節點上 `curl http://10.43.0.200:8081/healthz` 確認。
   - 不通就改用 B 段的備案。
   - 10.43.0.200 是否已被佔用，安裝前先用 `kubectl get svc -A` 查。
4. **audit webhook 的設定方式**：下游叢集是不是由 Rancher 建立的，我不知道，所以做法有兩條路：
   - **Rancher 建立的叢集**：設定要經 cluster 物件下發，手改節點會被 Rancher 蓋掉。
     - `machineSelectorConfig` 的 `audit-policy-file` 放的是 policy 內容，不是路徑。
     - webhook 設定檔與 token 檔可能要用 `machineSelectorFiles`，但它要在 local cluster 的 `fleet-default` 放一個 Secret，**這違反第 2 段**。
     - 遇到這情況先停下來問 Sam，不要自己決定。
   - **匯入或自建的叢集**：照 `deploy/rke2/README.md` 加一個 drop-in 設定，再重啟 rke2-server。
     - 用 `kube-apiserver-arg+`、`kube-apiserver-extra-mount+` 的寫法，以免蓋掉既有值。
   - **這一步做錯，kube-apiserver 會起不來。** lab 若只有一個 control plane，整座下游 API 會停。所以：
     - 先備份 `config.yaml` 與 `config.yaml.d/`。
     - 重啟後看 `journalctl -u rke2-server` 與 `crictl ps`。
     - 5 分鐘內 `kubectl get --raw /readyz` 沒有回 ok，就還原備份再重啟。
5. **儲存**：
   - chart 預設用叢集的 default StorageClass，但 RKE2 本身不附 StorageClass。
   - 先 `kubectl get sc`：
     - 沒有 default StorageClass 就用 `persistence.enabled=false`（emptyDir）。這是試吃版可以接受的做法，**不要另裝 provisioner**。
     - 有的話，SLES 16 是 SELinux enforcing，先 `getenforce` 確認狀態，Pod 起不來時看 `ausearch -m avc`。
6. **Helm chart 的 `lookup`**：產生 token 的寫法我沒法驗。
   - `helm template` 時 lookup 回空值，屬正常。
   - 實際安裝後確認 Secret `audit-loupe-ingest` 有值，且 `helm upgrade` 之後值不變。
7. **`machineSelectorConfig` 的格式**是我憑記憶寫的，請對照 Rancher 2.15 官方文件，不要照抄。

## 4. 目標（使用者看得到的行為）

1. **給簡報用的真實 log**：
   - 在測試 namespace 準備一個 1 個 replica 的 Deployment。
   - 請 Sam 在 Rancher UI 用 **Edit Config** 把 replicas 直接改成 3。不要用 +／− 按鈕，那會一按送一次。
   - 再用 Rancher 產生的 kubeconfig 執行一次 `kubectl apply` 修改 image。收齊下面四筆原始 JSON，存到 repo 的 `docs/samples/`：
   - 這兩個動作各一筆 Rancher API audit log。
   - 這兩個動作各一筆下游 K8s audit log。
   - 帳號、token、IP 等敏感值遮蔽後再存，遮蔽方式寫進報告。
2. **audit-loupe 試吃版可用**：Sam 在 Rancher 的下游叢集 Explorer 左側選單看到「Audit Loupe」，三頁都有真資料：
   - 「操作時間線」看得到上面兩個動作，帳號顯示 Rancher 使用者名稱，經由欄顯示 Rancher。
   - 「物件變更史」點那個 Deployment，看得到 replicas 1 → 3、image 舊 → 新，且作者正確。
   - 在 control plane 節點上用 `/etc/rancher/rke2/rke2.yaml` 執行一次 `kubectl label ns <測試 namespace> x=y`，「異常提示」出現「未經 Rancher，直接連 API server」。

## 5. 建議做法

### A. 開 Rancher API audit log（local cluster）

1. 用 local cluster 的 kubeconfig 取得目前的 Rancher helm values：`helm get values rancher -n cattle-system`。先存檔備份，存檔前遮蔽 bootstrapPassword 等敏感值。
2. 以 `--reuse-values`（或備份的 values）加上 `auditLog.enabled=true`、`auditLog.level=2` 做 helm upgrade。
   - **版本不變**，chart 版本照 `helm list` 現值帶入。
   - 先 `--dry-run`，把輸出的 manifest 和 `helm get manifest rancher -n cattle-system` 做 diff，確認只有 auditLog 相關的差異。`--dry-run` 本身不會顯示差異。
3. 讀取方式：`kubectl -n cattle-system logs -l app=rancher -c rancher-audit-log --prefix`。Rancher 可能有多個 replica，所以用 label，不要用 deploy/rancher。

### B. 部署後端到下游叢集

1. **建 image（ARM64）**：
   - `docker build -t <Sam 的 Docker Hub>/audit-loupe:0.1.0 backend/`，然後 push。用哪個帳號問 Sam。
   - image 先用 Docker Hub，取代 SUSE Private Registry。
   - registry.suse.com 若拉不到 BCI 映像，改用 `rust:1-slim` 加 `debian:stable-slim`，並在報告註明。
2. **驗 chart**：`helm lint deploy/chart`、`helm template deploy/chart`，有錯就修。
3. **安裝**：
   - `helm install audit-loupe deploy/chart -n audit-loupe-system --create-namespace`。
   - 帶入 `image.repository`、`clusterName=<下游叢集顯示名稱>`。
   - `persistence` 依第 3 段第 5 點決定。
   - `networkPolicy.allowFromCIDRs` 填 control plane 節點的 IP（/32）。
   - 確認 `/api/info` 的 `watchStatus` 每一項都是 watching 或 not served。
4. **設定 audit webhook 與 policy**（第 3 段第 4 點，先判斷叢集類型）：
   - 照 `deploy/rke2/README.md` 做。RKE2 原本的 audit log 檔照常保留。
   - token 寫入節點的方式：`kubectl get secret … -o jsonpath='{.data.token}' | base64 -d | ssh <節點> 'sudo tee /etc/rancher/rke2/audit-loupe.token >/dev/null && sudo chmod 600 /etc/rancher/rke2/audit-loupe.token'`。
     - `>/dev/null` 不可省，否則 tee 會把 token 印到終端機。
5. **ClusterIP 連不到時的備案**：Service 改 NodePort，webhook 指向 `http://127.0.0.1:<ingest 的 nodePort>`。
   - 先在節點上用 curl 確認連得到，再改設定。

### C. 載入 UI extension

1. 在 `ui/` 執行 `./node_modules/@rancher/shell/scripts/build-pkg.sh audit-loupe`，再執行 `yarn serve-pkgs`。
   - Node 版本要求 ≥24，用 `--ignore-engines` 或 nvm 處理，選哪個寫進報告。
2. Sam 在 Rancher 開啟 Preferences → Extension developer features，然後到 Extensions → Developer Load 填入 serve-pkgs 提供的網址。
   - 這一步需要 Sam 本人登入操作。CLI 只準備好網址與步驟，畫面上需要點的由 Sam 做。

### 最後手段

任何一段卡超過 40 分鐘：
- 停下來，把目前狀態、錯誤與已試過的方法寫進報告。
- A 段可以改由 Sam 在 Rancher UI 手動做，再請他貼 log。

不要為了自動化卡住整個下午。

## 6. 硬性限制

- 每一條 `kubectl`／`helm` 都要明確指定 `--kubeconfig`。local cluster 與下游叢集各用各的，報告寫出用了哪兩個檔案（只寫路徑）。
- token、密碼不寫進檔案、不 `echo` 到終端機。
  - 讀 `audit-loupe-ingest` token 要直接寫進節點上的 token 檔：用 B 段第 4 步的管線，不經過終端機輸出。
- Mac 本機不用 sudo。VM 內需要 root 的步驟（改 `/etc/rancher/rke2/`、重啟 rke2-server）可以在 VM 內 sudo。
- port-forward 一律綁定 127.0.0.1。
- 不給後端超出 chart 的權限：chart 內的 ClusterRole 是唯讀，且不含 Secret。
- 不在 local cluster 安裝任何東西。A 段的 helm upgrade 只改 auditLog 值。
- 不建立 GitHub repo、不 push 程式碼（專案名稱還沒定）。

## 7. 驗證（逐格回報，不接受「都通過」）

| # | 檢查 | 怎麼驗 | 不可以這樣驗 |
|---|---|---|---|
| 1 | Rancher audit log 在記錄 | 做第 4 段動作 1，從 `rancher-audit-log` 容器的輸出找出那兩筆的 `auditID` 與 requestURI | 不能只看容器 Running |
| 2 | 下游 policy 生效 | 節點上 `/var/lib/rancher/rke2/server/logs/audit.log` 出現那兩筆，level 為 Request 且有 requestObject | 不能只看 config.yaml 有參數 |
| 3 | webhook 送到後端 | 對後端呼叫 `/api/timeline?hours=1`（走 Rancher service proxy 或 port-forward），兩筆都在，`via` 是 `rancher`，`actor` 是 Rancher 使用者名稱 | 不能用後端 log 有 200 當證據 |
| 4 | 連得上變更內容 | `/api/history?…&name=<那個 Deployment>`：兩個 modified 版本對到兩位作者（UI 那一筆與 kubectl 那一筆），changes 分別是 replicas 1→3 與 image。若多出版本，寫出每一版的 change、actor 與 audit verb，不要只回報「多一版」 | 不能只驗第一個動作 |
| 5 | 直連會被標出 | 第 4 段動作 3 之後，`/api/alerts?hours=1` 出現 `bypassed_rancher` | |
| 6 | 反向：不該出現的沒出現 | 同一小時內，`/api/timeline`（預設，不帶 all=1）每一筆的 **`actor` 欄位**都不以 `system:serviceaccount:` 開頭 | 不要搜尋整份 JSON：經 Rancher 的每一筆，`caller` 欄位本來就是 `system:serviceaccount:cattle-impersonation-system:…` |
| 7 | Secret 內容沒進資料庫 | 用 kubectl create 建立一個含已知字串的測試 Secret，再 patch 一次。之後呼叫 `/api/timeline?hours=1&all=1&limit=2000` 與 `/api/objects?q=`，對回傳 JSON 搜尋該字串，應為 0 次；同時確認 timeline 裡**有**這兩筆 secrets 事件（否則 0 次只代表沒收到）。結束後刪除測試 Secret | 不能只搜尋 0 次就算過；runtime 映像沒有 shell 與 tar，不要用 kubectl exec／cp |
| 8 | UI 三頁 | Sam 在瀏覽器確認三頁都有資料。CLI 整理他回報的結果，截圖由 Sam 提供 | |
| 9 | 回歸 | OpenCost UI 與 cost-report 在任務前後各開一次，HTTP 200 且有內容；`kubectl get pods -A` 沒有新增的非 Running Pod（列出前後差異） | |
| 10 | 重啟不重複 | 後端重啟前後，**測試 Deployment** 沒有被改動。`kubectl rollout restart` 後端，再查它的 `/api/history`，不應多出新版本 | 其他物件在重啟期間可能真的被 Rancher agent 改到（例如 namespace 的 cattle.io/status），會正當地出現 modified_unseen，不算失敗 |

## 8. 回報格式

- 第 1 段待確認事項的實測結果：叢集 ID、叢集類型、kubeconfig 路徑、節點名稱，各寫明是哪一層的名字。
- 第 3 段每個假設：成立或不成立。不成立的話，實際長什麼樣、改了哪支程式。
- 第 7 段逐格結果。
- 做了但任務書沒提到的修改。
- 沒完成的部分卡在哪。
- 收尾清單：
  - 哪些設定要保留（Rancher auditLog、下游 audit webhook 都先保留，給 Sam 看）。
  - 要還原時的指令。
- 「任務書哪裡害我走冤枉路」一段。

報告裡不准出現密碼或 token。
