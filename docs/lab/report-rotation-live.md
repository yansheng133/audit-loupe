# 回報：在 lab 實測 audit log 輪替

執行：2026-10-21 00:21–00:36（台北）。對應：`docs/lab/overnight-1020-task.md` 第 4 項。lab 的 Audit Loupe 是 **0.3.0**（任務書寫 v0.3.1，實際查 `helm list` 與映像檔 tag 是 0.3.0；兩版的 collect-file 程式相同）。lab 沒有換成修正版。

## 一句話結論

缺陷在真的叢集上都重現了，而且比回放測試更常發生：輪替調成 1 MB 時，0.3.0 在收集元件**一直在跑**的情況下就無聲漏掉一整個檔（約 1,280 筆）；停機約 1 分鐘、期間輪替 3 次，5,100 筆只收到 658 筆。兩次畫面都顯示落後量 0、沒有任何收集中斷。所有設定都已還原。

## 名詞

| 名詞 | 意思 |
|---|---|
| 輪替 | apiserver 的 audit log 寫滿 `--audit-log-maxsize` 就改名成 `audit-<UTC 時間>.log`，再開新的 `audit.log`；超過 `--audit-log-maxbackup` 個舊檔就刪最舊的 |
| config.hash | kubelet 對 static Pod manifest 算的雜湊，manifest 內容一變就變 |
| 受影響時段 | 這次測試讓資料缺漏或異常的時間，之後看到那段的缺口要知道是測試造成的 |

## 受這次測試影響的時段（重要）

| 時段（台北，10/21） | 影響 |
|---|---|
| 00:22:55–00:35:07 | 下游 apiserver 輪替參數被改成 1 MB／2 個檔；這段時間的 audit log 不是平常的設定 |
| 00:23:00 前後約 20 秒、00:34:08–00:34:59 | 下游 apiserver 重啟兩次（改設定、還原設定） |
| 00:24:30–00:26:30 | 測試用的唯讀請求 10,000 筆（`system:admin` list `al-sample` 的 ConfigMap） |
| **00:26:07–00:26:10** | **一整個輪替檔沒被讀到**：約 1,280 筆不在 Audit Loupe 裡，沒有缺口紀錄 |
| **00:27:24–00:28:49** | **下游 collect-file 停機**；這段時間與停機期間寫入的大部分紀錄不在 Audit Loupe 裡（5,100 筆測試請求只存 658 筆），沒有缺口紀錄（停機不到 3 分鐘） |
| 00:28:59 | 測試用的唯讀請求 300 筆（`default` 的 ConfigMap），全部存入 |
| 00:31:06–00:33:59 | Rancher 換了兩次 Pod（改 `auditLog.maxSize`、還原），各約 1 分鐘無法使用 |
| 00:31:48–00:32:03 | 下游收集元件送資料與心跳收到 503（Rancher 換 Pod 期間），重送後恢復 |
| 00:32:30–00:33:10 | 測試用的未登入請求 6,000 筆打 Rancher `/v3/clusters`（都回 401），Rancher audit log 記成 `system:cattle:error` |

## 下游 RKE2（`c-m-26kbxk5f`）

### 1. 改之前的存證

節點上的 `kube-apiserver.yaml` 要 root 才讀得到，這次沒有 sudo，所以改用 kubelet 對 manifest 算的 `config.hash` 當同一份檔的證據。

| 項目 | 改之前 |
|---|---|
| kube-apiserver mirror Pod `config.hash` | `44b9eefd6b71660e1db8f9429ee1652d`（`config.source=file`） |
| audit 參數 | `--audit-log-maxage=30`、`--audit-log-maxbackup=10`、`--audit-log-maxsize=100`、`--audit-log-path=/var/lib/rancher/rke2/server/logs/audit.log`、`--audit-policy-file=/etc/rancher/rke2/audit-policy.yaml` |
| Rancher 叢集設定 `machineGlobalConfig` | `cni`、`disable-kube-proxy`、`etcd-expose-metrics`、`ingress-controller` 四項，**沒有任何 audit 設定**（audit policy 是節點本機設定的） |
| collector Pod | `audit-loupe-file-mk8qh`，重啟 1 次；位置 `{"inode":8307408,"offset":37305234}` |
| `/api/coverage` | 下游 `lagBytes: 0`，`gaps` 空 |
| 節點上的檔 | 3 個輪替檔（10/18、10/19、10/20 各約 100 MB）＋ `audit.log` 37 MB |

### 2. 改法

- 任務書寫「照 SA 量測那次的方式」，但那次其實是**用 ssh＋sudo 直接改節點上的 policy 檔**（`report-sa-volume.md`），跟同一句的「經 Rancher 的叢集設定改，不要手改節點上的檔」矛盾。我照後者：在 local 叢集的 `clusters.provisioning.cattle.io/rke2` 加
  `machineGlobalConfig.kube-apiserver-arg: ["audit-log-maxsize=1", "audit-log-maxbackup=2"]`。
- 20 秒後 apiserver 以新參數重啟。RKE2 把這兩個參數接在預設參數**後面**（同一個參數出現兩次，後面的生效），從之後的輪替大小（約 1 MB）證實生效。
- **改之前先備份節點上的 3 個舊輪替檔。**`maxbackup=2` 一生效就會刪掉它們，改回 10 也救不回來。備份在 Mac 的 `~/audit-loupe-lab/overnight/raw/`，權限 600、不進 repo，sha256 與節點上逐一相同。**果然 apiserver 一重啟，10/18、10/19 兩個就被刪了**，下一次輪替再刪掉 10/20 那個。

### 3. 執行中輪替（收集元件一直在跑）

- 用 `kubectl proxy`（只聽 127.0.0.1）送唯讀請求，每 1,500～1,700 個請求約 1 MB。5 批之間各停 20 秒；監看腳本每 15 秒記一次檔名、inode、大小、`lagBytes`、存入筆數（`~/audit-loupe-lab/overnight/rotation/monitor.log`）。
- 10/21 00:23–00:26 共輪替 **9 次**（`audit-2026-10-20T16-23-15.206` 到 `…16-26-10.880`）。inode 都是新的，**沒有看到 inode 重複使用**。

| 項目 | 結果 |
|---|---|
| 送出 | 10,000 筆（加上 2 筆測試連線） |
| Audit Loupe 存入 | **8,722 筆，少了 1,280 筆** |
| 原因 | 16:26:07.821 與 16:26:10.880 兩次輪替只隔 3 秒。收集元件 16:26:17 讀完 `…16-26-07.821.log` 後，直接換到**當時的** `audit.log`，中間那個 `…16-26-10.880.log`（inode 8334252，1,048,512 bytes）整個沒讀。collector log 有其他每個檔的 `switching to the new audit log finished=…`，**唯獨沒有這一個** |
| 畫面 | `lagBytes` 一直是 0，`gaps` 空，看起來完全正常 |

- 這是 PR 乙（`fix/collector-rotation`）順便修的「落後時連續輪替兩次，中間那個檔被跳過」，在真實叢集上重現了。修正版讀完一個輪替檔後，會依時間順序換下一個較新的輪替檔。
- 補充：16:24:55 與 16:24:58 也只隔 3 秒，那次剛好沒漏（收集元件 16:24:55.2 就讀完前一個）。漏不漏要看時機，所以是無聲、偶發的。

### 4. 收集元件停機期間輪替兩次以上

1. 存下 DaemonSet `audit-loupe-file` 原本的 spec。
2. 00:27:24 加上不存在的 `nodeSelector`（`audit-loupe-lab/never: "true"`），00:27:55 Pod 結束。
3. 停機期間送 3 批、共 5,100 筆唯讀請求（`kube-public` 的 ConfigMap），**輪替 3 次**。
4. 00:28:49 移除那個 `nodeSelector`，00:28:51 新 Pod `audit-loupe-file-zftmd` 開始跑。
5. 00:28:59 再送 300 筆（`default` 的 ConfigMap），確認恢復後照常收。

| 項目 | 結果 |
|---|---|
| 新 Pod 的 log | `following audit log … pos=Position { inode: 8334254, offset: 423068 }`，接著 `WARN starting the current file from the beginning old=8334254 new=8334344` |
| 停機期間 5,100 筆 | **只存 658 筆**：只有目前 `audit.log` 開頭那一段 |
| 跳過的 | 原本那個檔剩下的部分（已被 `maxbackup=2` 刪掉），以及**還在磁碟上的兩個輪替檔** `…16-28-21.486.log`、`…16-28-29.882.log` |
| 恢復後 300 筆 | 300 筆全部存入 |
| `/api/coverage` | `gaps` 空（停機約 1 分鐘，不到心跳判斷的 3 分鐘），`lagBytes` 0 |
| DaemonSet 還原 | 與存下的 spec 比對，只差 `generation`、`resourceVersion`、`deprecated.daemonset.template.generation`、`status`，都是 Kubernetes 自動維護的欄位 |

逐一對照缺陷：
- **缺陷 1（停機期間輪替兩次以上，中間的檔被跳過）**：重現。還在磁碟上的兩個輪替檔沒讀，只有一行 WARN。
- **缺陷 2（讀到一半的檔被刪，server 不知道）**：重現。原本那個檔沒讀完就被刪，server 沒有任何紀錄。
- **缺陷 3（落後量）**：重現。重新啟動後回報 `lagBytes: 0`，其實有兩個檔沒讀。
- **缺陷 4（inode 重複使用）**：**沒有重現**。這次 inode 都是遞增的新號碼。
- **修正版的行為**（PR 乙，沒有部署到 lab）：停機期間的 5,100 筆一筆都不會存入（不補讀），改成記一筆「沒有收集」（`collector-restart`），起點是 16:27:41 最後送出的那筆，終點是 16:28:49 啟動。這是原則 8 的新行為。

### 5. 還原

| 項目 | 結果 |
|---|---|
| Rancher 叢集設定 | 00:34:08 用 JSON patch 移除 `kube-apiserver-arg`（先用 `test` 確認值沒被別人改過）。`machineGlobalConfig` 與改之前 `diff` 為空 |
| apiserver | 51 秒後重啟完成。`config.hash` 回到 `44b9eefd6b71660e1db8f9429ee1652d`；audit 參數排序後與改之前 `diff` 為空 |
| Rancher 顯示下游 Ready | 00:35:07 True |
| collector | 沒有卡住。00:31:48–00:32:03 因為 Rancher 換 Pod，送資料與心跳收到 5 次 503，重送後恢復；位置跟著 `audit.log` 往前，`lagBytes` 0。**不需要重建 Pod** |
| 被刪掉的舊輪替檔 | 回不來（apiserver 自己刪的）。原檔備份在 Mac，見「做了但任務書沒提到的動作」 |

## Rancher 自己的 audit log（sidecar）

第 2 項 I 唯讀判斷不了（檔案還沒到 100 MB），所以照任務書改 Rancher：

| 步驟 | 時間 | 結果 |
|---|---|---|
| 存原值 | 00:30 | `helm get values` 只有 `auditLog`（`enabled`、`level: 1`、`destination: sidecar`）、`global`、`hostname`、`replicas`；chart `rancher-2.15.2`，revision 5 |
| 改 | 00:31:06 | 用本機的 `rancher-2.15.2.tgz`（同一版本）`helm upgrade --reuse-values --set auditLog.maxSize=1`，沒有加 `--wait`，改用 `kubectl rollout status` 等待 → revision 6，00:32:04 完成；新 Pod 的 `AUDIT_LOG_MAXSIZE=1` |
| 產生 log | 00:32:30 | 本機的 Rancher 通道不帶任何憑證打 `/v3/clusters`（回 401，Rancher 一樣會記），3,000＋3,000 筆 |
| 觀察 | 00:32:43、00:33:06 | **emptyDir 裡出現輪替檔**：`rancher-api-audit-2026-10-20T16-32-43.438.log`（1,048,138 bytes）。第二次輪替後它被刪，只剩 `…16-33-06.152.log`＋目前的檔，也就是 `maxBackup=1` 生效 |
| sidecar 有沒有跟上 | — | sidecar（`tail -F`）的輸出有 6,018 個不同的 auditID。磁碟上兩個檔的 1,843＋501 行**全部**在 sidecar 輸出裡（漏 0）。sidecar 自己的訊息：`… has been replaced; following new file` |
| 還原 | 00:33:28 | 用存下的原值 `-f`（同一個 chart）→ revision 7，00:33:52 完成。`helm get values` 與原值 `diff` 為空；新 Pod 的 `pod-template-hash` 是 `5d99c898b6`，跟改之前的 ReplicaSet 相同；`AUDIT_LOG_MAXSIZE=100`；兩個容器都 Ready；00:33:59 `/ping` 回 200 |

結論：
- **sidecar 模式照 `maxSize`、`maxBackup` 輪替。**`change-map-requirements.md` 3.6 (1) 的推論證實：預設 100 MB、只留 1 個舊檔、1 天。
- **sidecar 的 `tail -F` 會跟著輪替換檔**，這次沒有漏行。但 Audit Loupe 讀的是 sidecar 的容器輸出，所以還要再受 kubelet 對容器 log 的輪替影響（這次沒測）。
- **Rancher Pod 換掉時，emptyDir 跟著清空。**10/17 以來約 80 MB 的 Rancher audit log 在 00:31 這次換 Pod 時消失了。Audit Loupe 原本就只存其中的寫入、動作、登入，所以 Audit Loupe 的資料不受影響；但第 2 項 J 量的那些代理讀取紀錄，原檔已經不在了。

## 給 PR 乙與下一步的發現

1. **漏檔不只發生在停機時。**輪替很頻繁（大叢集、或 `maxsize` 小）時，收集元件一直在跑也會漏。PR 乙已處理。
2. **停機不到 3 分鐘就完全沒有缺口紀錄。**DaemonSet 換 Pod、節點重開這類短暫停機最常見。PR 乙改成由收集元件啟動時主動回報。
3. **`kube-apiserver-arg` 調輪替參數可行**，但參數會出現兩次（預設的在前、使用者的在後）。從 manifest 讀輪替參數時（3.6 (3) 第 5 點）要取最後一個。
4. **調小 `maxbackup` 會立刻刪掉現有的舊檔。**部署文件建議調整輪替參數時要寫這一點。

## 做了但任務書沒提到的動作

- 把節點上 3 個舊輪替檔原樣備份到 Mac（`~/audit-loupe-lab/overnight/raw/`，約 300 MB，權限 600）。理由：`maxbackup=2` 會刪掉它們。**Sam 決定要不要留**；裡面是原始 audit log，含 request body（Secret、ConfigMap 依 policy 不記 body）。
- 下游 Rancher 叢集設定用 JSON patch 的 `test` 確認值沒被別人改過才移除。
- 為了產生 Rancher audit log，送了 6,000 個不帶憑證的請求（沒有用任何 API key 或 token）。
- 啟動 `kubectl proxy`（`127.0.0.1:18001`、`18002`）與監看腳本，用完都已停掉。
