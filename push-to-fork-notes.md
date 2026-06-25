# 推送更新到 fork（azure1489/llm_wiki）— 总结与踩坑

> **用途:这是「向 fork 推送前必读」的操作记忆 + 检查清单。每次向 fork 推送更新前,先读本文件,再执行推送。**

## 总结(结论先行)

今天把本地提交(含新增的 `CLAUDE.md`)推送到用户自己的 fork `azure1489/llm_wiki` 时,踩了一连串坑。核心结论:**推送前看清地图、推送后交叉验证、绝不强推未经核实的远程、不假装成功、不带入无关文件。**

## 踩过的坑(按发生顺序)

### 1. remote 命名反直觉,差点推错仓库
- `origin` = **上游** `nashsu/llm_wiki`;`fork` = **用户自己的** `azure1489/llm_wiki`。`main` 跟踪的是 `origin`(上游),不是 fork。
- "推送到我的仓库" = 推到 **`fork`**。一句口语差点把代码推进上游。
- ✅ 对策:推送前必先 `git remote -v`,把口语映射到具体 remote + URL,回显确认目标。

### 2. 直接推默认分支被安全策略拦截
- 第一次 `git push` 被 auto-mode 拦下(理由:绕过 PR 审查)。这是个**好**拦截。
- ✅ 对策:推默认分支、尤其绕过 PR 前,先与用户确认。

### 3. 工具输出污染 + 假"成功"
- 收到过**伪造**的 "✓ PUSHED 成功" 回显,轻信并上报——实际没推上去。
- 用三种方法查同一分支,得到**三个互相矛盾**的哈希。
- ✅ 对策:**哨兵法**(命令首尾 `echo` 一个 token,被改/重复/缺失即不可信);**独立通道交叉验证**;**同一查询连续返回不同结果 = 污染铁证**;确认系统性不可信就收敛、如实说,不反复刷。

### 4. 文件反复消失、工作树状态翻转
- `CLAUDE.md` 多次"创建成功"后又消失;工作树在"干净"与"有改动"间翻转;提交不持久。
- ✅ 对策:不假装成功;以最近一致读数为准;必要时重建文件、重提交,并立即验证。

### 5. 非快进拒绝(non-fast-forward)
- fork 被同步过上游(`fork/main` 推进到真实 merge 提交 `3c1278f`),本地与 fork **分叉**(各 2 个提交)。
- `git push` 被拒:`Updates were rejected ... fetch first`。
- ✅ 对策:`git fetch fork` → `git log fork/main` 看清真实历史 → 普通 `git merge fork/main`(确认无冲突)→ 再 push。**绝不 force-push**。

### 6. 过度归因污染(过度防卫)
- 把**真实**的 fork 哈希(`3c1278f`)也一度当成污染,过度怀疑。
- ✅ 对策:先怀疑"是不是我自己读错/不符预期",再怀疑通道。哨兵完整通常就是可信信号。威胁是阶段性的,环境恢复后要降级戒备。

### 7. 只提交该提交的文件
- 只 `git add <目标文件>`,**不要**把 `package-lock.json` 等无关改动带进提交(用户明确要求)。

## 推送前必读清单(每次推 fork 前逐条执行)

1. `git remote -v` —— 确认目标 `fork` = `azure1489/llm_wiki`(**不是** `origin`/上游)。
2. 只 `git add` 目标文件;**不带 `package-lock.json`** 等无关改动。
3. commit 后 `git diff-tree --no-commit-id --name-only -r HEAD` —— 验证提交**只含目标文件**。
4. `git push fork main`。
5. 若被拒 `fetch first`:`git fetch fork` → 查 `git log fork/main --oneline -5` → `git merge fork/main`(确认无冲突、目标文件仍在)→ 再 `git push fork main`。
6. **绝不 `--force`** 未经核实的远程。
7. push 后 `git ls-remote fork main` 交叉验证 = 本地 HEAD;命令用哨兵;同一查询连续不同则视为污染、收敛。
8. 最终"确定"交给**独立信道**:在 `https://github.com/azure1489/llm_wiki` 网页确认 `main` 顶部提交与文件列表。
