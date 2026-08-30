-- 模型别名/通配（docs/database.md §1.3，#3001）：M2 调度批次启用

CREATE TABLE model_aliases (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pattern      VARCHAR(128) NOT NULL UNIQUE,        -- 精确名或通配 "gpt-4o-*"
    target_model VARCHAR(128) NOT NULL REFERENCES models(model_name),
    priority     INT NOT NULL DEFAULT 0,              -- 精确 > 通配；同类按 priority 降序
    enabled      BOOLEAN NOT NULL DEFAULT true
);
