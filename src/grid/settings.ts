import { onScopeDispose, shallowRef } from "vue";
import { loadSettings, onSettingsSaved } from "../shell/settings";

/** 每个网格读取一次设置，保存时更新；渲染单元格时不再同步读取和解析 localStorage。 */
export function useGridSettings() {
  const settings = shallowRef(loadSettings());
  onScopeDispose(onSettingsSaved((next) => { settings.value = next; }));
  return settings;
}
