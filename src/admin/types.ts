import type { SequenceInfo } from "../catalog/types";

/**
 * 数据库级动作（R5）：字段名与 Rust 侧 ddl::model::Action 的 camelCase 序列化一致（T9 定义）。
 * foreign_keys 走 setForeignKeys —— 它与结构编辑共用同一个动作，执行后目录里的外键状态同步更新。
 */
export type DatabaseAction =
  | { kind: "setForeignKeys"; enabled: boolean }
  | { kind: "setPragma"; pragma: string; value: string }
  | { kind: "attachDatabase"; path: string; name: string }
  | { kind: "vacuum" }
  | { kind: "reindex"; target: string | null }
  | { kind: "setEventEnabled"; event: string; enabled: boolean }
  | {
      kind: "alterSequence";
      sequence: string;
      start: number | null;
      increment: number | null;
      minValue: number | null;
      maxValue: number | null;
      cycle: boolean | null;
    };

/** 面板上按引擎出现的分节；不在清单里的分节不渲染，也不发请求。 */
export interface AdminSections {
  pragma: boolean;
  attached: boolean;
  maintenance: boolean;
  extensions: boolean;
  sequences: boolean;
  events: boolean;
  partitions: boolean;
}

/** 序列参数草稿：文本原样保存，空串表示“这一项不改”。 */
export interface SequenceDraft {
  increment: string;
  minValue: string;
  maxValue: string;
  start: string;
  cycle: boolean;
}

/** 序列草稿的来源快照，供 sequenceAction 判断哪一项真的改了。 */
export type SequenceSource = Pick<SequenceInfo, "increment" | "minValue" | "maxValue" | "startValue" | "cycle">;
