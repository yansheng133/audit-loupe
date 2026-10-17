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

```bash
scripts/build-images.sh 0.2.0 --push
```

產生 `docker.io/yansheng133/audit-loupe-server` 與 `audit-loupe-collector`（同一個程式，以子命令區分角色）。離線環境請推到內部 registry，安裝時改 `image.*.repository`。

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

Rancher 的 Extensions 頁面加入 Audit Loupe 的 extension repository 後安裝（repository 由 `ui/` 的 GitHub Actions 產生）。安裝後：

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

## 9. 移除

```bash
helm uninstall audit-loupe -n audit-loupe-system
```

PVC（`audit-loupe-data`）不會自動刪除，要保留稽核資料請先備份 `/data/audit-loupe.db`。下游叢集節點上的讀取位置記錄在 `/var/lib/audit-loupe`，可以手動刪除。
