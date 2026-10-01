# 结算价格版本一致性核对

2026-09-30，修正已实现并完成下述固定源码回归，整体核心功能验证仍进行中。历史单位识别读取旧 speech 端点和快照时，发现音频、视频与自定义透传使用已经生成的 Quote，但写账时重新读取当前 PriceBook 的 epoch。上游等待期间发生价格发布时，旧金额/快照被标为新版本。自定义透传失败还保留预报价的 original/discount/list_price，把失败消费计入原消费或优惠。

新约束见 DESIGN §3.4：账单版本取实际报价快照，热更新不得只改标签；失败释放预扣后的四金额按零消费记账，尝试快照可保留。保持现有报价时点、Token/字符数量和成功收费，旧账本不重写。

新增 `gateway_pricing_epoch` 使用有进入信号和放行门闩的实际 HTTP 上游：请求报价后以生产热更新使用的 `PriceBookHandle::swap_if_newer` 原语切换 41 → 42（价格十倍），再让上游返回；检查当前请求仍使用原报价、下一请求使用新报价。覆盖 speech 的 ratio/per_call、转写、翻译、视频秒数和自定义透传成功/失败。这是网关请求与价簿切换的并发验收，不代表管理端发布事务、NATS 通知和所有实例热更已被本测试覆盖。

修正后的基线 `/tmp/okapi-pricing-epoch-baseline-corrected.log` 实际退出 **101**，7 failed。6 项成功路径均出现 PG/outbox/CH 的 epoch=42，而同一报价快照 epoch=41；失败透传也出现版本冲突。失败透传有 0.5 个人倍率：实际消费、钱包变动都是 0，但 PG/outbox/CH 分别写入原消费 5,000、优惠 2,500 micro；新价请求又写入 50,000 / 25,000。基线前后 1,172 个后端/构建文件一致。首次基线 `/tmp/okapi-pricing-epoch-baseline.log` 实际 101，还包含“只充值 Redis 却断言 PG 余额”和“透传错误响应没有 request-ID 头”的测试夹具问题；保留该失败记录，不把它们归为生产余额缺陷。修正后通过持久充值入口初始化 PG/Redis，透传错误通过独立测试用户的已落账记录定位请求。

实现移除三个写账函数的当前价簿读取，版本取实际报价快照。失败透传的 original/discount/list_price 归零，尝试快照保留；上游成本沿用失败 PG NULL、outbox/CH 0 与 unknown 的既有契约。未改变成功收费、字符/Token 采集、钱包池归属或退款重试。

首轮修正 `/tmp/okapi-pricing-epoch-fixed-first.log` 实际退出 **0**，7 passed、0 failed/ignored/filtered/软跳过；源文件前后 1,172 个一致。固定金额：speech ratio 22 → 220、speech 按次 7,000 → 70,000、转写/翻译 6,000 → 60,000、视频 4 秒 40,000 → 400,000、成功透传 5,000 → 50,000 micro；失败两次均为 0。交叉断言 PG 快照、outbox、实际 CH 明细和 `mv_user_day` 金额/优惠/成本/错误/Token 汇总，钱包 PG/Redis、事件变动和 API Key 用量；同一 outbox 在 CH 投递两次仍只有一条请求。字符 speech 的 11 字符保留，Token 为 0。首轮工作区/all-targets Clippy `-D warnings` 实际 0，未新增 lint 放行。

固定副本 `/tmp/okapi-pricing-epoch-snapshot-20260930` 包含 1,172 个后端/构建文件，复制前后、全部回归后和测试容器内逐文件校验一致。相比阶段起点只改变音频、视频、自定义透传三个生产文件及新增集成测试；domain/pricing/ledger 和统计查询/schema 未变化。

完整财务三包 `/tmp/okapi-pricing-epoch-snapshot-financial.log` 实际退出 **0**：**188 passed、0 failed/ignored/filtered/软跳过**，17 个完整块，包含 5 项既有对拍。报告 `...-financial-report.json` 无解析错误或未结束块，日志 SHA-256 `af31ec2ac48b063471c204021d1d57ccb18eae0d7dcbaae3845ef69351970677`。

完整已选关联套件 `/tmp/okapi-pricing-epoch-snapshot-related.log` 实际退出 **0**：单次 **168 passed、0 failed/ignored/filtered/软跳过**，10 个完整块（lib 98、分析 17、日志 15、门户归属 1、音频 7、透传 5、新并发 7、价格规则 12、视频 3、CH 投递 3）。报告 `...-related-report.json` 无解析错误或未结束块，日志 SHA-256 `3fc48ed313b4ca7feb2f76ac0000fee91e1e9a5cc828ae4285953a48f1507224`。独立个人中心页面 `/tmp/okapi-pricing-epoch-snapshot-portal-pages.log` 实际退出 **0**，9 passed、0 failed/ignored/filtered/软跳过；此前选中的 `console_portal` 只有归属一项，不能把它说成个人中心九项。

固定源码工作区/all-targets Clippy `-D warnings` `/tmp/okapi-pricing-epoch-snapshot-clippy.log` 实际退出 **0**。格式、金额浮点守卫、错误码守卫、API 源清单和差异空白检查通过；汇总器/API 清单 Python 单测 16 项单列。首次 API 清单检查缺少 `--check` 的路径参数而退出 2，补全参数后实际 0，未修改源清单来绕过。188、168、9、首轮 7 和 Python 16 是独立运行，不相加为完整工作区验收。统计 30 项本轮未重跑，其查询/schema/test 文件与上一输出速度阶段 191 项固定版本一致，本轮的实际失败零金额明细与用户汇总由新增并发测试验证。

使用原有独立 PG `okapi_character_units_20260930`（正式迁移至 0027）、自己的 Redis/CH 服务，各夹具自建自清 CH 数据库；没有提交、推送、部署、共享服务重启或前端改动/构建。本轮后续按用户要求停止向其他聊天发送协调消息。

历史 TTS 主 Token 校准仍待继续：旧端点和快照确有记录，但缺失单位、快照版本冲突或已过期明细不能猜模型/价格来补造单位。本阶段先修复可复现的价格证据一致性，不宣称已校准所有旧统计。
