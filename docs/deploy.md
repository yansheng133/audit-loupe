# 部署 Audit Loupe

一個 Helm chart（`deploy/chart`）涵蓋所有角色，每座叢集用 values 決定要裝哪些部分。namespace 一律用 `audit-loupe-system`，UI 會到這個 namespace 找後端。

## 1. 每座叢集裝什麼

| 叢集 | server | collect-watch | collect-rancher | collect-file |
|---|---|---|---|---|
| Rancher 所在叢集（local） | 預設放這裡 | 要，含 Rancher 權限物件（`rancherTypes: true`） | 要 | local 是 RKE2 時可選 |
| Rancher 建立的 RKE2 下游叢集 | 不要 | 要 | 不要 | 要 |

- **server**：存資料、給 UI 讀。整個環境只裝一座。
- **collect-watch**：物件版本（改了什麼）。每座要稽核的叢集都裝。
- **collect-rancher**：Rancher API audit log（經 Rancher 的操作與 Rancher 本身的管理動作）。只裝在 Rancher 所在叢集。
- **collect-file**：RKE2 kube-apiserver audit log（包含繞過 Rancher 的操作）。裝在 RKE2 叢集，跑在 control plane 節點上。
  - 只送「收集元件在跑的時候」讀到的紀錄。收集元件重新啟動時，從目前 `audit.log` 的結尾開始讀；停機期間寫進去的紀錄（含停機期間輪替出去的檔）**不會事後補讀**，那段時間在收集狀態頁記成「沒有收集」，原因 `collector-restart`。起點是「停機前最後送出的那筆紀錄」與「最後一次確認已經讀完」兩者中較晚的時間（叢集很安靜時，不會把整段安靜時間算成沒收集；最多高估約 30 秒）。
  - **第一次安裝**（`collector.file.onFirstStart`）：
    - `fromNow`（預設）：從目前 `audit.log` 的結尾開始收，並送一筆「開始收集」的標記（`/api/coverage` 的 `starts`，`detail` 為 `fromNow`），表示這個節點從這一刻起才有資料。
    - `readExisting`：把目前的 `audit.log` 從頭讀進來，標記的 `detail` 為 `readExisting`，`fromMs`～`toMs` 這段的紀錄是**安裝時才讀進來的既有紀錄，不是即時收到的**。開啟前要知道：
      - 已經輪替出去的舊檔不讀，所以安裝前的紀錄可能不完整。
      - 第一次啟動會一次讀進最多約 100 MB（RKE2 預設單檔上限），server 與網路會有一段尖峰。
  - **狀態目錄遺失不等於第一次安裝**：節點上的 `/var/lib/audit-loupe` 被刪掉或遺失時，收集元件會先問 server 有沒有這個節點的紀錄（心跳或缺口）。有的話當成重新啟動：從現在開始讀，從 server 最後一次收到這個節點心跳的時間到現在記成「沒有收集」，原因 `collector-state-lost`。
  - 收集元件在跑的時候發生輪替，會把輪替出去的檔讀完，再依序讀較新的檔。正在讀的檔一直開著，所以就算它被改名或刪掉（`--audit-log-maxbackup`），也會讀完，不會漏。
  - 收集元件落後時，還沒開始讀的輪替檔先被刪掉，會立刻記一筆「沒有收集」（`file-removed`），從那個檔輪替的時間到下一個還在的檔的第一筆。被就地改寫的檔記成 `file-replaced`。都不用等心跳中斷 3 分鐘。
  - 認檔不只看 inode，也比對檔案開頭的雜湊，而且都從同一個開啟的檔讀，所以 inode 被重複使用時不會從舊位置接著讀。
  - 輪替出去的檔最後一行不完整時，送給 server 擱置，不會無聲丟掉。
  - log 裡有一行讀不懂時，server 只把那一行擱置（記節點、檔名、位置、長度與 sha256，不存內容），同一批的其他行照常存入。`/api/coverage` 的 `rejectedCounts` 有各收集元件被擱置的行數，`/api/rejected` 列出每一行（不含原文）。重送同一批不會重複記。
  - 「沒有收集」有兩種，`/api/coverage` 都會回傳、不互相合併：`kind: heartbeat`（server 發現收集元件 3 分鐘以上沒有回報）與 `kind: reported`（收集元件自己回報的）。同一段時間可能兩種都有。
  - 收集元件回報「沒有收集」失敗時會退避重送，server 收到重複的回報只留一筆。
  - `AL_HEARTBEAT_SECS`（心跳間隔，預設 60 秒）最大 120 秒：server 把 180 秒沒有心跳當成中斷。
  - **升級順序**：先升 server，再升收集元件。舊版 server 不認得收集元件的新回報（`gap` 來源與 `/ingest/v1/known`），會一直回 400，收集元件第一次啟動時會一直重試等待。

收集元件一律推送到 server，不需要 server 連回各叢集。

## 2. 事前準備

### 2.1 Rancher 開啟 API audit log

```bash
helm upgrade rancher rancher-prime/rancher -n cattle-system --reuse-values \
  --set auditLog.enabled=true \
  --set auditLog.level=1 \
  --set auditLog.destination=sidecar
```

- level 1 記 metadata 與 header（含 User-Agent，用來分辨介面或 kubectl）。不需要 request body。
- `destination` 要用 `sidecar`，collect-rancher 讀的是 `rancher-audit-log` 這個 sidecar 的輸出。
- 改這些值會讓 Rancher Pod 重建（從 chart 推論，待 lab 驗證中斷多久）。

### 2.2 RKE2 開啟 apiserver audit log

policy 在 `deploy/policy/rke2-audit-policy.yaml`：只記變更（含 request body）、Secret 與 ConfigMap 的存取（只記 metadata，不記內容）、exec／attach／port-forward；讀取與背景流量不記。

- **Rancher 建立的 RKE2**：在叢集的 `spec.rkeConfig.machineGlobalConfig` 加 `audit-policy-file`，值是 policy 檔的**內容**（不是路徑）。Rancher 會把檔案送到節點並依 upgrade strategy 逐台重啟。
- **自建 RKE2**：把 policy 存成 `/etc/rancher/rke2/audit-policy.yaml`，在 `/etc/rancher/rke2/config.yaml` 加 `audit-policy-file: /etc/rancher/rke2/audit-policy.yaml`，逐台重啟 `rke2-server`。
- 不要改用 `kube-apiserver-arg` 傳 policy：RKE2 不會幫你設定 log 路徑與輪替。

### 2.3 映像檔

`docker.io/yansheng133/audit-loupe-server` 與 `audit-loupe-collector`（同一個程式，以子命令區分角色），amd64 與 arm64 都有，由 GitHub Actions 在推版本 tag 時發佈。

離線環境或要推到內部 registry 時，在目標架構的機器上自行建置：

```bash
REPO=registry.example.com/audit-loupe scripts/build-images.sh 0.3.0 --push
```

安裝時改 `image.*.repository`。

### 2.4 每座叢集一把 token

```bash
openssl rand -hex 32   # 每座叢集各產生一把
```

server 用 token 判斷資料來自哪座叢集，不採信收集元件自己報的名稱。叢集 ID 用 Rancher 的 ID（`local`、`c-m-xxxxx`），UI 才對得上叢集名稱。

## 3. 安裝 Rancher 所在叢集（server ＋ 收集元件）

`values-local.yaml`：

```yaml
server:
  enabled: true
  tokens:
    existingSecret: audit-loupe-tokens   # 內容見下方
  ingress:                               # 下游叢集的收集元件從這裡送資料
    enabled: true
    host: audit-loupe-ingest.example.com
    tlsSecret: audit-loupe-ingest-tls
collector:
  ingest:
    url: http://audit-loupe.audit-loupe-system.svc:8081   # 同叢集走內部位址
  token:
    existingSecret: audit-loupe-collector-token          # key: token，值是 local 那把
  watch:
    enabled: true
    rancherTypes: true
  rancher:
    enabled: true
```

```bash
kubectl create namespace audit-loupe-system
# server 認得的 token：每行「<叢集 ID> <token>」
kubectl -n audit-loupe-system create secret generic audit-loupe-tokens \
  --from-literal=tokens="$(printf 'local %s\nc-m-xxxxx %s\n' "$TOKEN_LOCAL" "$TOKEN_PROD")"
kubectl -n audit-loupe-system create secret generic audit-loupe-collector-token --from-literal=token="$TOKEN_LOCAL"
helm install audit-loupe deploy/chart -n audit-loupe-system -f values-local.yaml
```

server 用 SQLite 存在 PVC 上，預設 5Gi、保留 30 天。RKE2 沒有內建 StorageClass，請先準備一個，或指定 `server.persistence.storageClass`。

### 3.1 PVC 要多大

每筆紀錄連同索引約 750 位元組（lab 資料實測：每筆 736 位元組，加上變更地圖的索引約 752 位元組）。過了保留期的紀錄刪除後，空間給新紀錄用，但檔案不會變小，所以以「保留天數加一天」估：

PVC ≥ 每天筆數 × 750 位元組 × (保留天數 ＋ 1) × 1.3（預留給 WAL 與成長）

| 每天筆數 | 30 天 | 90 天 |
|---|---|---|
| 約 6.4 萬（lab：2 座叢集） | 約 1.8 GiB | 約 5.3 GiB |
| 約 71 萬（lab 的 11 倍，效能測試的量） | 約 20 GiB | 約 59 GiB |

**取決於設定**：每天筆數取決於 apiserver audit policy 記哪些層級、叢集與工作負載的數量，請用自己環境的量重新算。server 已在跑的話，`GET /api/info` 的 `retentionDays` 是目前的保留天數，PVC 用量可以用 `kubectl exec` 看 `/data` 的大小。

PVC 建好後要放大，StorageClass 必須支援擴充（`allowVolumeExpansion: true`）；不支援的話（例如 local-path），一開始就要給夠。

## 4. 安裝下游 RKE2 叢集（只裝收集元件）

`values-downstream.yaml`：

```yaml
collector:
  ingest:
    url: https://audit-loupe-ingest.example.com
    caSecret: audit-loupe-ca      # ingest 用私有 CA 時；Secret 的 key 為 ca.crt
  token:
    existingSecret: audit-loupe-collector-token   # 這座叢集自己那把
  watch:
    enabled: true
  file:
    enabled: true
```

```bash
kubectl create namespace audit-loupe-system
# collect-file 要掛 hostPath（唯讀讀取 audit log），namespace 要允許
kubectl label namespace audit-loupe-system pod-security.kubernetes.io/enforce=privileged
kubectl -n audit-loupe-system create secret generic audit-loupe-collector-token --from-literal=token="$TOKEN_PROD"
helm install audit-loupe deploy/chart -n audit-loupe-system -f values-downstream.yaml
```

- RKE2 以 CIS profile 安裝時，Pod Security 由 admission 設定檔統一套用 restricted，需要把 `audit-loupe-system` 加進該設定的豁免清單（待 lab 驗證）。
- collect-file 以 root 身分唯讀讀取 audit log（檔案權限 0600），但不帶任何 capability、根檔案系統唯讀。
- 每座叢集的收集元件要能連到 server 的 ingest 位址（HTTPS 443），防火牆請放行。

## 5. 轉送 SIEM 與防竄改

server 在寫入時把每座叢集的紀錄串成雜湊鏈，每小時記一次鏈的最新位置（錨點）。在 `server.forward` 設定轉送目標後，整理過的紀錄與錨點會一起送出：

```yaml
server:
  forward:
    syslog: tls://siem.example.com:6514   # 或 splunkHec、webhook，可同時設定
    caSecret: siem-ca                     # 私有 CA 時
```

- 送出的是整理過的紀錄（身分鏈、叢集、物件、風險規則、雜湊），不是原始 log。syslog 為 RFC 5424，內容為 JSON；中、高風險為 warning，其餘為 notice。
- 每個目標各自記錄送到哪裡，目標暫時無法連線時會累積、恢復後補送。`/api/forwarding` 看得到積壓量與最後一次錯誤。
- **雜湊鏈要搭配外部的錨點才有意義**：鏈存在同一個資料庫裡，能改資料庫的人也能重算整條鏈。錨點送到 SIEM（或其他不能事後修改的地方）之後，可以用 `/api/integrity/anchor?cluster=&event_id=&hash=` 比對現在的資料是否與當時一致。沒有設定轉送時，雜湊鏈只能偵測意外損毀。
- `/api/integrity` 逐座叢集驗證整條鏈，回報第一筆對不上的紀錄。

## 6. SUSE Security（選用）

在裝有 SUSE Security 的叢集加上 `collector.neuvector`，exec 事件的證據欄會列出之後在那個 Pod 裡被判定為違規的程序。

```yaml
collector:
  neuvector:
    enabled: true
    apiKeySecret: audit-loupe-nv-apikey   # key: apikey，值為「<access key>:<secret>」
    caSecret: audit-loupe-nv-ca           # controller 憑證（預設是自簽）
```

- API key 只需要「Security Event」讀取權限，可以限定 namespace。
- SUSE Security 只記錄判定為違規或可疑的程序（Monitor／Protect 模式下不在允許清單的程序、內建的可疑程序偵測），**不是完整的指令紀錄**。Discover 模式下不會產生這類事件。畫面上一律寫「SUSE Security 判定為違規的程序」。
- controller 只保留最近的事件，收集元件每 30 秒讀一次。

## 7. UI extension

UI extension 的原始碼在 [yansheng133/audit-loupe-ui](https://github.com/yansheng133/audit-loupe-ui)，由 Rancher 官方的 extension workflow 發佈成 Helm repository。

1. 在 Rancher 的 `Apps` → `Repositories` 加入 `https://yansheng133.github.io/audit-loupe-ui`。
2. 到 Extensions 頁面安裝 Audit Loupe。

安裝後：

- 左上選單的全域應用多一個「Audit Loupe」，開啟變更地圖。
- 每座叢集的 Dashboard 多一張卡片，點了直接以該叢集篩選開啟。
- server 不在 local 時，第一次開啟會詢問 server 所在的叢集 ID，記在這台瀏覽器。

## 8. 誰看得到稽核資料

UI 透過 Rancher 的 service proxy 讀 server，所以由 Rancher 的權限決定：要有 server 所在叢集 `audit-loupe-system` namespace 的 `services/proxy` get 權限。給稽核人員的 Role 範例：

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: audit-loupe-viewer
  namespace: audit-loupe-system
rules:
  - apiGroups: [""]
    resources: ["services/proxy"]
    resourceNames: ["http:audit-loupe:8080", "audit-loupe"]   # 實際比對的名稱待 lab 驗證
    verbs: ["get"]
```

再以 RoleBinding 綁給稽核人員的使用者或 IdP 群組。

只有 `get` 的人可以看所有頁面，但不能啟動「重新計算」（見第 9 節）；要讓他能啟動，再加上 `create`。

## 9. 調整規則

規則的開關、嚴重度、不適用的範圍寫在 values 的 `server.rules`，沒寫到的維持預設。完整的規則清單與欄位說明在 `values.yaml` 的註解，介面「規則」頁也看得到目前生效的設定。

```yaml
server:
  rules:
    pod_exec:
      severity: high
      except:
        - namespaces: ["dev-*"]
          reason: 開發環境允許除錯
          until: "2026-12-31"     # 選填，到那天結束為止
    bypassed_rancher:
      except:
        - users: ["break-glass-*"]
          clusters: ["c-m-xxxxx"]
          reason: 緊急帳號，事後由資安覆核
    secret_read:
      enabled: false
```

`helm upgrade` 之後 server 重新啟動才會套用。

**設定有誤時升級會失敗，原本的 server 照常運作。**每次安裝或升級前，chart 會先用新的映像檔跑一個 Job（`audit-loupe-config-check`）讀一次規則、白名單、時區與變更時段，讀不過就讓 `helm upgrade` 失敗，叢集上的東西都不會換。原因看這個 Job 的 log：

```bash
kubectl -n audit-loupe-system logs job/audit-loupe-config-check
# Error: AL_RULES: pod_exec.except[0]: reason is required
```

改好 values 再升級一次即可（失敗的 Job 會在下次升級前自動刪掉）。這個檢查可以用 `server.configCheck.enabled: false` 關掉；關掉之後設定有誤時，server 會停在啟動失敗，直到改好為止（server 的 Deployment 是先停舊的再起新的）。

套用之後：

- 新進的紀錄用新設定判斷。
- 之前的紀錄維持原本的判斷。「變更地圖」與「規則」頁會出現提示，說明有幾筆、預估要多久，按「重新計算」才會用新設定重算全部紀錄。期間可照常使用，原始紀錄與雜湊鏈不受影響。
- 已轉送到 SIEM 的警示不會重送。
- 「規則」頁保留每次設定變更與重新計算的時間。

### 9.1 身分分類與白名單

規則只看「人」做的事。每個身分（名稱加上 Rancher 使用者 ID）在第一次出現時分類一次，存在 server 的身分表，分成五類：人、叢集元件、ServiceAccount、Rancher 內部、未知。內建的判斷：

| 身分 | 分類 |
|---|---|
| `kube-system` 的 ServiceAccount、`system:` 開頭的名稱（`system:admin` 除外）、名稱或 User-Agent 是 cloud-controller-manager | 叢集元件 |
| `cattle-` 開頭 namespace 的 ServiceAccount（含 Fleet、NeuVector） | Rancher 內部 |
| 其他 namespace 的 ServiceAccount | ServiceAccount |
| Rancher 使用者的 principal 有 `system://`（叢集佈建用的帳號） | Rancher 內部 |
| `system:admin`（節點上的管理者憑證） | 人（對不到是誰） |
| `system:cattle:error`（Rancher 未通過驗證的請求） | 未知 |
| 其他 | 人 |

內建判斷認不出來的（例如只能靠憑證名稱認的元件），寫在 `server.identities`：

```yaml
server:
  identities:
    - names: ["rke2-cloud-controller-manager"]   # 名稱或 Rancher 使用者 ID，可用 * 
      class: component                            # human、component、serviceaccount、rancher、unknown
      reason: 用憑證登入的 RKE2 元件，只能靠名稱認   # 必填，畫面上的「身分說明」
```

- 精確名稱優先於有 `*` 的名稱；都符合時以寫在前面的為準。有 `*` 的名稱最多 1,000 個；只有 `*` 的名稱不接受。
- 白名單和規則一樣算進設定代碼：改了之後，之前的紀錄維持原本的判斷，「規則」頁會提示重新計算（第 9 節）。
- 每個身分目前的分類與依據：`GET /api/identities`。
- 存下來的紀錄本身不會改（`human` 欄位是雜湊鏈的一部分），改的是身分表。

**從 0.3.x 升級時**：Rancher 叢集佈建用的帳號（例如 `u-xxxxx`，principal 為 `system://`）以前被當成人，現在歸「Rancher 內部」，「非上班時間」「繞過 Rancher」這類警示會變少。升級後到「規則」頁執行一次重新計算，舊紀錄才會照新的分類判斷。

## 10. 移除

```bash
helm uninstall audit-loupe -n audit-loupe-system
```

PVC（`audit-loupe-data`）不會自動刪除，要保留稽核資料請先備份 `/data/audit-loupe.db`。下游叢集節點上的讀取位置記錄在 `/var/lib/audit-loupe`，可以手動刪除。刪掉後若 server 還留著這個節點的紀錄，收集元件會當成重新啟動，從現在開始讀，並把中間那段記成「沒有收集」（`collector-state-lost`）；要當成全新安裝，server 端也要一起移除。

## 11. 升級注意事項

### 從 0.5.0 升級

- server 多一個 emptyDir 掛在 `/tmp`（`server.tmpSize`，預設 1Gi），給 SQLite 的暫存檔用。0.5.0 在唯讀的根目錄上建不起變更地圖的索引，升級後第一次啟動會在背景建好。

### 從 0.4.0 升級

- **第一次啟動會建立變更地圖用的索引。**紀錄在 20 萬筆以內時當場建，啟動多花幾秒；超過時在背景建，期間變更地圖照常可用，只是比較慢，畫面上方會註明「索引建立中」。建索引的時間：lab 的 83 萬筆實測約 1 秒；每天約 71 萬筆、保留 30 天（約 2,100 萬筆）依比例估約半分鐘，實際取決於磁碟速度。
- **第一次啟動會從既有紀錄回填身分表**（每個名稱加 Rancher 使用者 ID 一筆），並依內建判斷分類。存下來的紀錄本身不改，雜湊鏈不受影響。
- **介面要一起升級。**新介面接舊後端時，時間範圍只把範圍外的列調暗、件數不變，理由顯示後端原文；舊介面接新後端可以正常使用，只是看不到新欄位。
- 設定白名單（`server.identities`）會改變設定代碼，「規則」頁會提示重新計算，見 9.1。
- 升級前會先檢查設定（第 9 節），設定有誤時升級失敗、原本的 server 照常運作。
- values 沒有必須改的項目。
