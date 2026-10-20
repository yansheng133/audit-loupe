# 回報：lab 收尾

執行：2026-10-21 00:36–00:38（台北）。對應：`docs/lab/lab-cleanup-task.md`（依 `overnight-1020-task.md` 修改：保留 `al-lab-*` 使用者與 `audit-loupe-viewer`）。

## 一句話結論

任務書要清的東西，**10/18 19:10–19:20 規則 lab 收尾時就已經清掉了**：規則已經是預設值並重新計算過，`al-rules-*` 已經刪除。這一輪只刪了 repo 裡的空鎖檔，沒有 `helm upgrade`。另外，要保留的兩個測試帳號與 `audit-loupe-viewer` 也已經在 10/18 被刪掉了，**沒辦法照 10/20 的決定保留**，要不要重建由 Sam 決定。

## 第 2 節：確認結果

| 不確定的地方 | 結果 | 出處 |
|---|---|---|
| `al-rules-*` 在哪座叢集 | 在**下游**，10/18 19:10:25 已被刪。兩座叢集現在都是 NotFound | Audit Loupe 紀錄：`system:admin` 用 kubectl（darwin）`delete namespaces al-rules-a`、`al-rules-b` |
| 測試帳號怎麼建的 | Rancher 使用者（`users.management.cattle.io`）：`al-lab-member`＝`u-pb5mp`、`al-lab-noproxy`＝`u-z6j6q`，10/17 14:31:57 建立。**10/18 19:19:45、19:20:05 已被刪**（`admin` 經 Rancher API，UA `curl/8.7.1`）。現在 local 叢集查不到這兩個 User，也查不到它們的 token 與綁定 | Audit Loupe `users` 表的 lifetimes；`versions` 的 User deleted；`kubectl get users` |
| `audit-loupe-viewer` 在哪 | local 叢集 `audit-loupe-system` 的 Role `audit-loupe-viewer` 與 RoleBinding `audit-loupe-viewer-lab`。**10/18 19:20:05 已被刪**；兩座叢集現在都沒有叫這個名字的 Role／RoleBinding | `versions` 表；`kubectl get roles,rolebindings -A` |
| server 目前的 values | `helm get values`：**沒有 `server.rules`**（只有 cluster、enabled、ingress、persistence、rancherCluster、tokens）。`/api/rules` 的 12 條規則都等於預設、沒有例外 | `helm get values`（遮掉 secret 名稱）；`/api/rules` |

我在哪裡猜錯：任務書第 1 節列的四項「還沒清的東西」，有三項其實已經清了，帳號那項也已經不在。任務書的現況寫在清理之前。

## 每一項動作

| 項目 | 之前 | 做了什麼 | 之後 |
|---|---|---|---|
| A 規則改回預設 | 已是預設：設定代碼 `560d162e0034822d`，與第一版相同。設定紀錄 3 筆：預設 → 測試（`becf0d3407264a19`）→ 預設 | **沒做**：values 已經沒有 `server.rules`，`helm upgrade` 不會改任何東西，還會讓 server 重啟。選比較保守的做法 | 不變 |
| A 重新計算 | 第 3 次重新計算（settingsId 3）在 10/18 19:11:14 完成，50,868 筆，沒有錯誤；`pending` null | 沒做（不需要） | 不變 |
| B 刪 `al-rules-a`、`al-rules-b` | 兩座都 NotFound | 沒做（不需要） | NotFound |
| C 保留 `al-lab-*` 與 `audit-loupe-viewer` | 已不存在（10/18 被刪） | **沒有重建**：建立使用者不在授權範圍內 | 不存在，見「需要 Sam 決定」 |
| D 保留 `al-sample`、錄下的 log | `al-sample` 的 3 個 Deployment 是 **replicas 1**，不是任務書寫的 0。這是 10/19 Sam 要我「把環境先開回來」時改的，Mac 上的 external-tool 也在跑 | 沒動 | 不變，見「需要 Sam 決定」 |
| E `.git/stale-maintenance-lock-claude` | 0 bytes，10/19 07:34 | 確認沒有 git 行程在跑後刪除 | 不存在 |

## 第 5 節驗證

| # | 驗證 | 結果 |
|---|---|---|
| 1 | 規則回到預設；固定期間 `secret_read` 127 筆、警示總數 1,484 | `/api/rules`：12 條都等於預設。`/api/alerts?from=2026-10-16T16:00:00Z&to=2026-10-18T09:00:00Z`：有警示的紀錄 **1,484** 筆；其中 `secret_read` **127** 筆（另有 off_hours 1,354、bypassed_rancher 193、rbac_changed 66、pod_exec 3、credential_issued 1、namespace_deleted 1，一筆可以有多個警示）。兩個數字都一致。「設定紀錄多一筆」不適用：這一輪沒有改設定 |
| 2 | 重新計算做完 | `pending` null、`job` null；最後一次重新計算 `finishedMs` 有值、`error` null |
| 3 | 刪掉的真的不見 | `al-rules-a`、`al-rules-b` 在 local 與下游都是 `NotFound`（4 次 `get`） |
| 4 | 不該刪的還在 | `al-sample` 的 namespace 在，`app-config`、`ci-runner`、`pod-watcher` 三個 Deployment 都在（**replicas 1**，不是 0，見上）。Audit Loupe：local 的 server、rancher、watch，下游的 file、watch 都在跑。laoban extension（`laoban-ext` 0.1.20）在 |
| 5 | 資料沒掉 | `/api/integrity`：下游 verified 227,969、local 1,059，兩座 `first_bad_id` 都是 null。最後一筆紀錄：下游 2026-10-20T16:37:45Z、local 2026-10-20T16:35:01Z（台北 10/21 00:37、00:35）。這一輪沒有 upgrade，所以這是「現在還在收」的證據 |
| 6 | 保留的帳號還在 | **不在**。`al-lab-member`、`al-lab-noproxy`、`audit-loupe-viewer` 都在 10/18 被刪了（見第 2 節） |

## 需要 Sam 決定

1. **要不要重建 `al-lab-member`、`al-lab-noproxy` 與 `audit-loupe-viewer`**，給權限覆核頁的測試用。建立使用者與密碼要 Sam 自己做，或明確授權。
2. **`al-sample` 要維持 1 份，還是 scale 回 0**；Mac 上的 external-tool 要不要停。目前都在跑，10/20 一天約 6,300 筆 audit 紀錄來自它們。

## 任務書沒提到但做了的事

- 用 Audit Loupe 自己的紀錄（`events`、`versions`、`users` 表的唯讀副本）查出這些東西是誰、什麼時候刪的。
- 沒有做 `helm upgrade`，所以 server 沒有重啟。

## 任務書哪裡讓我走冤枉路

- 第 1 節的現況已經過時（寫在 10/19，但 10/18 晚上就清完了），第 3 節 A–C 都跟著不成立。下次寫任務書前，先用 `/api/rules`、`kubectl get ns` 確認一次。
- 「保留 `al-lab-*`」的決定（10/20）晚於刪除的時間（10/18）。
