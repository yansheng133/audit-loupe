# Audit Loupe

在 Rancher 裡回答「誰、在什麼時候、透過什麼管道、對哪個物件、改了什麼」。
Answers *who changed what, when, and how* across Rancher-managed Kubernetes clusters, inside the Rancher UI.

**狀態：第一版（M1）開發中。** 目前的程式是從試吃版（M0）搬入並改名，只用合成資料驗證過。
**Status: M1 in progress.** The code is the M0 tasting version moved in and renamed; verified with synthetic data only.

- 設計：[`docs/design.md`](docs/design.md)
- 介面模型（示意資料，瀏覽器直接開）：[`mock/change-map.html`](mock/change-map.html)

## 第一版的做法 / M1 approach

| 環境 | 是誰 | 改了什麼 |
|---|---|---|
| Rancher 本身（local cluster） | Rancher API audit log | 物件版本監看 |
| Rancher 建立的 RKE2 下游叢集 | 下游 kube-apiserver audit log | 物件版本監看 |

- 收集元件把紀錄推送到後端，每座叢集一把 token；後端位置預設 local cluster，可以改放其他叢集。
- 後端在寫入時把畫面要用的資料整理好，查詢只讀資料庫，不重讀原始 log。
- 介面是 Rancher 的全域頁面，經 Rancher service proxy 存取後端，看資料的權限由 Rancher RBAC 決定。
- 每座叢集都標出收集涵蓋度，看不到的部分會明講。

- Collectors push to the backend with a per-cluster token; the backend runs in the local cluster by default and can be moved.
- Data is shaped at write time; the UI queries the database, never the raw logs.
- The UI is a Rancher global page that reaches the backend through the Rancher service proxy, so Rancher RBAC controls who can see audit data.
- Every cluster shows what it does and does not collect.

## 組成 / Layout

| 路徑 | 內容 |
|---|---|
| `backend/` | Rust（axum、kube-rs、SQLite）。`cargo test`；`audit-loupe seed-demo` 產生示意資料 |
| `ui/pkg/audit-loupe/` | Rancher UI extension（UI extensions v3） |
| `deploy/chart/` | Helm chart（第一版會改寫） |
| `mock/` | 介面設計模型 |
| `docs/archive/m0/` | 試吃版的 webhook 做法與 lab 任務書，僅供參考 |

## 授權 / License

Apache-2.0
