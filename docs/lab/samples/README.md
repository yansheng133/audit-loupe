# M1+M2 lab 原始紀錄樣本

同一個動作（在 Rancher 介面把下游 Deployment `al-lab-test/al-demo` 的副本數改成 2，2026-10-17 06:25:21Z）分別在兩邊留下的紀錄，各一行，排版成多行方便閱讀。

處理方式：

- IP 換成 RFC 5737 範例位址（192.0.2.x、198.51.100.x）。
- 節點主機名稱換成 `node-1.example.com`。
- Cookie、權杖編號（`requesttokenid`、`credential-id`）、`X-Api-Csrf`、`X-Api-Cattle-Auth`、`X-Api-Schemas`、`X-Api-Url-Prefix` 換成 `<masked>`。
- `authorization.k8s.io/decision` 和 `reason` 也被遮了：遮罩規則看鍵名含 `authorization` 就遮，這兩個其實不是機密（值是 `allow` 和「RBAC 允許的原因」）。

## rancher-audit-ui-scale.json（Rancher audit log，等級 1）

| 欄位 | 意思 |
|---|---|
| `auditID` | Rancher 自己的請求編號，和下游的 `auditID` 不同，兩邊對不起來 |
| `requestURI` | Steve 路徑 `/k8s/clusters/<叢集 ID>/v1/apps.deployments/<命名空間>/<名稱>`，資源名稱是複數 |
| `user.name` | Rancher 使用者 ID（`user-xxxxx`） |
| `user.extra.username` | 登入名稱（這裡是 admin） |
| `user.extra.principalid` | 身分來源＋ID，本機帳號是 `local://` |
| `user.extra.requesthost` | 使用者連進來的主機名稱（這裡是 SSH 通道的 localhost:8443） |
| `user.extra.requesttokenid` | 這次使用的權杖編號（已遮） |
| `method` | HTTP 方法；介面改副本數是 PUT |
| `remoteAddr` | 連到 Rancher 的來源位址（經通道所以是節點自己） |
| `responseCode` | 回應碼 |
| `requestTimestamp`／`responseTimestamp` | 只到秒 |
| `requestHeader.User-Agent` | 瀏覽器是 Mozilla…，kubectl 是 `kubectl/vX` |
| `requestHeader.Referer` | 介面頁面網址，可看出是從哪一頁按的 |
| `requestHeader.X-Api-Csrf` | **Rancher 沒有遮這個值**，原樣寫進 audit log（這裡是我們遮的） |
| `responseHeader.Warning` | apiserver 的警告，例如介面多送的 `_summaries` 欄位 |

沒有 `user.identity`，也沒有 `impersonatedBy`。等級 1 不含請求內容，看不到改了什麼。

## rke2-audit-ui-scale.json（下游 kube-apiserver audit log，等級 Request）

| 欄位 | 意思 |
|---|---|
| `auditID` | apiserver 的請求編號 |
| `verb` | `update`（介面送 PUT） |
| `user.username` | 實際連線的身分：`system:serviceaccount:cattle-system:cattle`（cattle-cluster-agent） |
| `user.extra.authentication.kubernetes.io/pod-name` | 發出請求的 agent Pod |
| `impersonatedUser.username` | Rancher 代為執行的使用者 ID，和 Rancher 端 `user.name` 相同 |
| `impersonatedUser.extra.username` | 登入名稱，和 Rancher 端相同 |
| `impersonatedUser.extra.requesttokenid` | 權杖編號（已遮）；Rancher 把使用者的 extra 一起帶過來，所以兩邊都有這個欄位 |
| `sourceIPs` | agent Pod 的位址，不是使用者的位址 |
| `userAgent` | agent 的 UA，不是瀏覽器的；要知道是介面還是 kubectl，得看 Rancher 端 |
| `objectRef` | 資源、命名空間、名稱、resourceVersion |
| `requestObject` | 送出的整個物件（等級 Request 才有），可看出 `spec.replicas: 2` |
| `requestReceivedTimestamp`／`stageTimestamp` | 到微秒 |
| `annotations.authorization.k8s.io/*` | 授權結果（已遮，見上） |
