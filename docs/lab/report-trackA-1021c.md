# 報告：10/21 第三批軌 A——套用修正、更新本機 server、用真實資料驗證

依據：`docs/lab/parallel-1021c-task.md` 的「軌 A」。執行時間：2026-10-21 17:07 開始。

**一句話結論**：兩個補丁照步驟套上，測試、lint、build 都通過；本機 server 更新後，用真實資料確認上一批找到的問題已修好——用 `get` 進入容器的 Pod 現在顯示為 `exec`，也有「是否啟用 SUSE Security 收集」那一項；沒有紀錄的物件回 `{"error":"no-records"}` 404。

名詞：
- **補丁**：Claude 在雲端先寫好的程式修改檔，這一軌只負責比對、套用、驗證。
- **detail**：點一列時右側面板的內容，由 `/api/changemap/detail` 單獨提供。
- **dependsOn**：會影響這個物件紀錄能看到什麼的設定。

## 步驟對照表

| 步驟 | 做了什麼 | 證據 | 結果 |
|---|---|---|---|
| A-1.1 | `git status` | 主工作目錄只有 `docs/lab/parallel-1021c-task.md` 一個新檔（當時在 `docs/decisions-1021b` 分支） | 完成 |
| A-1.2 | `git fetch origin`，`git switch -c docs/task-1021c origin/main` | origin/main 是 `d20e66a`；新檔跟著帶過去 | 完成 |
| A-1.3 | 只 commit 任務書，`git push -u origin docs/task-1021c`，開 PR | commit `dd889b1`（1 個檔），PR #15，標題照任務書 | 完成 |
| A-1.4 | 不刪任何分支 | 沒有刪 | 完成 |
| A-2.1 | `git worktree add ../audit-loupe-a -b fix/changemap-v01-followup d20e66a…` | `HEAD is now at d20e66a` | 完成 |
| A-2.2 | clippy 基準 | `~/audit-loupe-lab/patches/trackA/clippy-main.txt`：主程式 **19**、測試 **21** | 完成 |
| A-3 | 比對 SHA-256，`git am` | `c2ae196a…a0fd1`，等於 README；commit `f0e8c24`（`api.rs`、`changemap.rs`、`docs/api/changemap-v0.md`） | 完成 |
| A-4.1 | `cargo test --locked` | 單元 114 通過、2 略過；整合 10 通過、1 略過；0 失敗 | 完成，一致 |
| A-4.2 | `cargo clippy --all-targets` | 主程式 19、測試 21，和 A-2 相同 | 完成，沒有變多 |
| A-4.3 | `git push -u origin fix/changemap-v01-followup`，開 PR | PR #16，標題照任務書，描述貼 README 的「後端」表格 | 完成 |
| A-5.1 | 確認 UI 工作目錄 | 分支 `feat/v5`；HEAD `4469c8dbff4092bd1a09c2cfcfe5c1666da87e88`；`git status` 乾淨 | 完成 |
| A-5.2 | 比對 SHA-256，`git am` | `b5925598…50dcf5`，等於 README；commit `d268bcb`（4 個 v5 檔） | 完成 |
| A-6 | `yarn lint`、`yarn build-pkg audit-loupe`，push | 用 SUSE BCI Node 24 容器（和 `scripts/dev.sh` 同一個映像檔與 digest）。lint：`eslint --max-warnings 0 .` 結束碼 0。build：結束碼 0，只有原本就有的 3 個 webpack 大小建議（301 KiB）。push：`4469c8d..d268bcb` | 完成 |
| A-7.1 | `cargo build --release --locked` | `Finished release` | 完成 |
| A-7.2 | `deploy-bin.sh <執行檔路徑>` | 印出 `both up`；腳本沒有改 | 完成 |
| A-8.1 | 沒紀錄的物件 | `{"error":"no-records"} 404` | 完成，通過 |
| A-8.2 | 找 10/20 有進入容器的 Pod | 10/20 就有：1 列，`<下游叢集>`、`audit-loupe-system`、`audit-loupe-file-mk8qh`、verb `get`、子資源 `exec`、3 筆 | 完成 |
| A-8.3 | 那個 Pod 的 detail | `ops` 3 筆，verb 都是 `exec`（資料庫裡是 `get`）；`dependsOn[0].setting` = 「是否啟用 SUSE Security 收集」；`ops` 不是空的，不需要 `system=1` | 完成，通過 |
| A-8.4 | 整頁回應 `select=<key>` 的 `detail` | `ops`（3 筆）與 `dependsOn` 都和 A-8.3 相同；整個 `detail` 也完全相同 | 完成，通過 |
| A-9 | dev server 能啟動 | 照上一批做法直接執行 `scripts/dev.sh`（`PORT=8006`、`NAME=audit-loupe-ui-a-dev`、`API` 填假網址、`AL_LOCAL_API=http://host.docker.internal:18090`）：`Compiled successfully in 13047ms`，之後 `docker stop`；沒有登入 Rancher。`touch ~/audit-loupe-lab/perf/.trackA-done` | 完成 |
| A-10.1 | 報告 commit、push | 本檔 | 完成 |
| A-10.2 | CI 結果寫進報告後再 push | 見下面「CI 結果」 | 見下方 |
| A-10.3 | 告訴 Sam 怎麼看介面 | 見最後一節；沒有替 Sam 開 | 完成 |

## 卡住的地方

沒有。

## 任務書沒寫但我做了的事

1. **A-8.3 多查了一次 `system=1`。**任務書只在 `ops` 是空的時候才要加。這次 `ops` 不是空的，我仍多查了一次，結果相同（3 筆 `exec`、同一個 `dependsOn`）。只讀，沒有影響；A-8.4 照任務書沒有加 `system=1`。
2. **commit 前用 grep 掃 IP**（只讀）：任務書與本報告都沒有真實 IP。
3. **套用前看了補丁的內容與改動的檔案**（`git apply --stat --check`，只讀）。
4. **A-6 的 node_modules 用匿名 volume。**lint 與 build 在同一個容器裡依序執行，node_modules 放在匿名 volume，容器結束（`--rm`）時一起移除。這樣不會寫到 Sam 的 dev server（8005）正在用的 `audit-loupe-ui-node-modules`，也不會留下新的 volume。上一批用的是一個暫時的具名 volume，事後再刪掉；這次不用刪任何東西。npm 與 yarn 的快取 volume 沿用 `scripts/dev.sh` 的兩個。
5. **為了寫 A-10.3，用 `docker inspect` 看了 8005 那個容器掛哪個目錄、有沒有設定 `AL_LOCAL_API`**（只讀；只看那兩項，沒有印出其他環境變數）。

## 任務書可能寫錯的地方（沒有造成損害，照做）

- 第 0 節第 2 點寫「不改 API 約定」，但後端補丁本來就改了 `docs/api/changemap-v0.md`（新增「v0.1 補充」一節），README 也寫了這一點。我照 A-3 套用，沒有另外改約定。

## A-8 的回應摘要

| 檢查 | 預期 | 實際 |
|---|---|---|
| `detail?day=2026-10-20&key=none//pods/none/none` | `{"error":"no-records"} 404` | `{"error":"no-records"} 404` |
| 資料庫裡那個 Pod 的進入容器紀錄 | verb 是 `get` 或 `create` | `get`／`exec`，3 筆 |
| detail 的 `ops` | 有 `exec` | 3 筆都是 `exec`，執行者類別是 `human` |
| detail 的 `dependsOn[0].setting` | 是否啟用 SUSE Security 收集 | 是否啟用 SUSE Security 收集 |
| 整頁 `select=` 的 `detail` 與 detail API | `ops`、`dependsOn` 相同 | 相同，整個 `detail` 也相同 |

上一批（`report-proto-changemap-v01.md` 第 3 點）同一類 Pod 的 `dependsOn` 是 `[]`，操作顯示成 `get`。

## CI 結果

- PR #16（`fix/changemap-v01-followup`，`f0e8c24`）：`helm chart` 通過；`backend tests` 執行中（第一次寫報告時）。
- UI PR #4（`feat/v5`，`d268bcb`）：`Find Extensions to Build` 通過、`Test build audit-loupe` 通過。

## 在瀏覽器看介面（Sam 自己開）

本機 server（18090）已經是這一軌的新版。在 UI repo 用既有的 `scripts/dev.sh`：

```sh
cd ~/audit-loupe-ui-v5
PORT=8006 NAME=audit-loupe-ui-v5-dev API=https://<Rancher 網址> AL_LOCAL_API=http://host.docker.internal:18090 scripts/dev.sh
```

然後開 `https://127.0.0.1:8006`（自簽憑證），登入 Rancher，再進 Audit Loupe 的 v5 頁面。

- 8005 已經有一個 `audit-loupe-ui-dev` 容器在跑，但它掛的是 `~/audit-loupe-ui`（不是 v5 的工作目錄），也沒有設定 `AL_LOCAL_API`，所以看不到這一版。上面的指令用 8006 和另一個容器名稱，可以和它並存；看完按 Ctrl-C 就會停掉並移除。
- 第一次啟動要先 `yarn install`，大約需要 1 分鐘，看到 `Compiled successfully` 才能開。
- 可以看：資料提示列有沒有把版面往下推；點一個有 exec 的 Pod（例如 10/20 `audit-loupe-system` 的 collect-file Pod），右側面板要有「是否啟用 SUSE Security 收集」。
