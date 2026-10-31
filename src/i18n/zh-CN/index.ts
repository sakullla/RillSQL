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

/** zh-CN 字典（默认语言）：按域拆分文件，域内按界面分组嵌套。 */
export const zhCN = {
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
