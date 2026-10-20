/**
 * 命令面板的“执行”是一次全局请求：请求可能在目标查询标签挂载之前发出。
 * 用单调递增的消费水位确保每个请求只执行一次，不会因为切换标签而重放。
 */
let consumed = 0;

export function consumeExecuteRequest(value: number | undefined): boolean {
  if (value == null || value <= consumed) return false;
  consumed = value;
  return true;
}

export function resetExecuteRequest(): void {
  consumed = 0;
}
