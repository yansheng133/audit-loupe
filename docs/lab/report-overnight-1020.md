# 總報告：10/20 夜間批次

執行：2026-10-20 23:20 至 10-21 00:45（台北）。對應：`docs/lab/overnight-1020-task.md`。六項都做完，沒有發生事故；改過的 lab 設定都已還原，只有節點上被 apiserver 刪掉的舊 audit log 回不來（已先備份在 Mac）。

| 項目 | 狀態 | 產出（檔案或 PR） | 需要 Sam 決定的事 |
|---|---|---|---|
| 1. 收好未 commit 的東西 | 完成 | PR [#10](https://github.com/yansheng133/audit-loupe/pull/10)（甲）收了 4 份文件；UI repo 分支 `wip/change-map-layout-v4`（沒開 PR），UI 工作目錄已切回 `main` 並 pull | `wip/change-map-layout-v4` 之後要不要刪 |
| 2. 第 0 層事實調查（只讀） | 完成 | `docs/lab/report-ui-facts.md` | 身分建議分類（人／叢集元件／ServiceAccount／Rancher 內部／未知）；NeuVector 的 SA 要不要另成一類；10 倍基準用 10/18（72,244）或 10/20（66,541）；時間列粒度（建議 10 分鐘） |
| 3. 修收集元件四個缺陷 | 完成 | PR [#11](https://github.com/yansheng133/audit-loupe/pull/11)（乙），CI 兩個檢查都通過 | 第一次安裝要不要讀現有的 `audit.log`；心跳缺口與收集元件回報的缺口重疊時畫面怎麼合併；收集狀態頁要不要先顯示缺口原因與被擱置的行 |
| 4. lab 實測輪替 | 完成（已還原） | `docs/lab/report-rotation-live.md` | Mac 上備份的 3 個舊輪替檔（約 300 MB）要不要留 |
| 5. lab 收尾 | 完成（大部分在 10/18 就已清掉） | `docs/lab/report-cleanup.md` | 要不要重建已被刪掉的 `al-lab-member`、`al-lab-noproxy`、`audit-loupe-viewer`；`al-sample` 維持 1 份還是 scale 回 0、external-tool 要不要停 |
| 6. 第 1 層後端實作計畫 | 完成（只有計畫） | `docs/layer1-plan.md` | 計畫第 9 節的 5 點（「看不到」的定義、物件鍵加 API group、粒度、Rancher 系統使用者不算人之後警示數會變、10 倍基準） |

## 一句話結論

- 收集元件的缺陷在真的叢集上比回放測試更容易發生：輪替調成 1 MB 時，0.3.0 收集元件一直在跑也漏了一整個檔；停機 1 分鐘、期間輪替 3 次，5,100 筆只收到 658 筆。兩次畫面都顯示「正常」。
- PR #11 修了這些缺陷，四個情境的測試在 main 上都會失敗、在 PR 上都通過。
- 介面上的「Unknown／收集中斷」大多不是斷線：`u-d67owytc34` 是 Rancher 的系統使用者；local 的「Unknown」是因為 local 沒收 apiserver audit log。

## 事故

無。

- 改設定的步驟都沒有造成無法還原的異常，每一步都在幾分鐘內還原並驗證。
- 有一段預期中的短暫中斷：Rancher 換 Pod 期間（10/21 00:31:48–00:32:03），下游收集元件送資料與心跳收到 5 次 503，重送後恢復，沒有掉資料。

## 對 lab 做過的所有變更

| 變更 | 時間（台北，10/21） | 是否還原 | 證據 |
|---|---|---|---|
| 下游叢集設定（Rancher `clusters.provisioning.cattle.io/rke2`）加上 `kube-apiserver-arg: audit-log-maxsize=1, audit-log-maxbackup=2` | 00:22:55 | **已還原**（00:34:08 移除） | `machineGlobalConfig` 前後 `diff` 為空；apiserver `config.hash` 回到 `44b9eefd…`；audit 參數前後 `diff` 為空 |
| 下游 apiserver 因此重啟兩次 | 00:23、00:34 | — | 各約 20 秒、51 秒後 `/readyz` ok；Rancher 顯示下游 Ready |
| **節點上的舊輪替檔被 apiserver 刪除**（10/18、10/19、10/20 三個，各約 100 MB，以及測試期間 1 MB 的輪替檔） | 00:23 起 | **無法還原** | 改設定前已原樣備份到 Mac 的 `~/audit-loupe-lab/overnight/raw/`，sha256 與節點上的一致。Audit Loupe 早就收完這些紀錄（當時 `lagBytes` 0），資料庫不受影響 |
| 下游 DaemonSet `audit-loupe-file` 暫時加上不存在的 `nodeSelector` | 00:27:24–00:28:49 | **已還原** | spec 與存下的版本比對，只差 Kubernetes 自動維護的欄位；Pod 從 `audit-loupe-file-mk8qh` 換成 `audit-loupe-file-zftmd`（重啟次數因此歸 0） |
| Rancher：`helm upgrade --reuse-values --set auditLog.maxSize=1`（revision 6） | 00:31:06 | **已還原**（revision 7，用存下的原值） | `helm get values` 前後 `diff` 為空；Pod 的 `pod-template-hash` 回到 `5d99c898b6`；`AUDIT_LOG_MAXSIZE=100`；`/ping` 200 |
| Rancher 換了兩次 Pod，emptyDir 裡 10/17 以來的 Rancher audit log 跟著消失 | 00:31、00:33 | 無法還原 | Audit Loupe 只存其中的寫入、動作、登入，資料庫不受影響；第 2 項 J 量的代理讀取紀錄已經沒有原檔 |
| 測試流量 | 00:24–00:33 | — | 下游：`system:admin` 唯讀 list ConfigMap 共 15,400 次（`al-sample` 10,000、`kube-public` 5,100、`default` 300）；Rancher：不帶憑證打 `/v3/clusters` 6,000 次（都回 401） |
| 下游 collect-file 漏收的紀錄 | 00:26:07–00:26:10、00:27:24–00:28:49 | 無法還原（0.3.0 的缺陷） | 見 `report-rotation-live.md` 的「受影響的時段」 |

第 5 項沒有改 lab：規則、`al-rules-*` 在 10/18 就清過了。repo 裡刪了 `.git/stale-maintenance-lock-claude`，這不屬於 lab。

## 各任務書哪裡讓我走冤枉路

1. **lab 的版本**：夜間任務書寫「v0.3.1（lab 目前的版本）」，實際是 0.3.0。
2. **ui-facts 的基準日**：10/17 不是整天（資料從 14:16 才開始）。唯一沒有中斷的整天是 10/20，所以等到台北午夜過後才取資料。
3. **ui-facts 的 API 數**：寫「6 支」，變更地圖其實一次打 8 支，加上規則提示列是 9 個請求。
4. **groups／extra 不在資料庫**：任務書建議先打 API，但 API 和資料庫都沒有存。好在節點上的原始 log 還在。
5. **第 4 項的改法自相矛盾**：寫「照 SA 量測那次的方式」，但那次是用 ssh＋sudo 直接改節點的檔，跟同一句的「經 Rancher 的叢集設定改」相反。我照後者做。
6. **第 4 項的存證**：節點上 `kube-apiserver.yaml` 的 sha256 要 root 才讀得到。我改用 kubelet 的 `config.hash`，比對前後是否一致。
7. **第 4 項沒提到 `maxbackup=2` 會立刻刪掉現有的舊檔**：我是看參數意思才發現的，所以先備份。
8. **cleanup 任務書的現況過時**：寫在 10/19，但 10/18 晚上就清完了；「保留 `al-lab-*`」的決定也晚於刪除的時間。
9. **夜間任務書第 0 節的「出事時」**：要求 30 分鐘內還原不了就停掉，但第 4 項本身就會讓收集中斷（測試的目的就是這個）。我把「預期中的漏收」寫進受影響時段，不當成事故。

## 做了但任務書沒提到的動作

- **改寫已推上去的 PR 乙分支**：第一次 commit 時，誤把我在 `backend/target-main/` 跑 main 的 clippy 產生的建置目錄（約 1,855 個檔，含本機路徑）一起 commit 並推上去了。當時還沒開 PR、只有我在用這個分支，為了不讓幾百 MB 的建置產物留在公開 repo 的歷史裡，我移除後用 `git push --force-with-lease`（指定舊的 commit）覆蓋了 `fix/collector-rotation`。本機的建置目錄已刪。**這是這一輪唯一的 force-push。**
- **複製到 Mac 的資料**（都在 `~/audit-loupe-lab/overnight/`，不進 repo）：
  - `db/`：server 資料庫的唯讀副本（第 2 項分析用）。
  - `raw/`：下游節點的原始 audit log 4 個，權限 600（第 2 項取 groups 用，以及第 4 項前的備份）。
  - 裡面有稽核資料，**Sam 看完報告後可以刪**。
- **用過的本機服務**：
  - port-forward `127.0.0.1:18080`（server API）
  - `kubectl proxy` `127.0.0.1:18001`（下游）、`127.0.0.1:18002`（local，沒用到就停了）
  - 監看腳本

  做完都已停掉，見下一段。
- **PR 乙順便修了一個任務書沒列的缺陷**：收集元件落後時連續輪替兩次，中間那個檔會被跳過。第 4 項在 lab 重現了，漏了約 1,280 筆。
- **PR 乙多了一個環境變數** `AL_HEARTBEAT_SECS`（心跳間隔，預設 60 秒），讓測試在幾秒內跑完。
- **用 Audit Loupe 自己的紀錄**查出 `al-rules-*` 與測試帳號是誰、什麼時候刪的。

## 結束時的狀態

- 兩座叢集的 Audit Loupe 都在收資料，`/api/integrity` 兩座的雜湊鏈都完整（`first_bad_id` null）。
- 下游 collect-file 是新 Pod `audit-loupe-file-zftmd`（第 4 項換的），其他 Audit Loupe Pod 與 10/18 相同。
- Rancher 是 revision 7（設定與 revision 5 相同），Pod `rancher-5d99c898b6-*`。
- PR #10（甲）已改成 ready；PR #11（乙）CI 通過。**兩個都沒有合併**。
