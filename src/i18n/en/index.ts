import type { MessageSchema } from "../index";
import { common } from "./common";
import { errors } from "./errors";
import { shell } from "./shell";
import { connections } from "./connections";
import { catalog } from "./catalog";
import { query } from "./query";
import { grid } from "./grid";
import { structure } from "./structure";
import { exportDict } from "./export";
import { history } from "./history";
import { compare } from "./compare";
import { diagram } from "./diagram";
import { redis } from "./redis";
import { mongo } from "./mongo";

/** en 字典：结构与 zh-CN 逐 key 对齐（MessageSchema 约束，缺 key 编译报错）。 */
export const en: MessageSchema = {
  common,
  errors,
  shell,
  connections,
  catalog,
  query,
  grid,
  structure,
  export: exportDict,
  history,
  compare,
  diagram,
  redis,
  mongo,
};
