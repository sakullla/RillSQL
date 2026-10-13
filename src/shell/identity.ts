import { invoke } from "@tauri-apps/api/core";
import { PRODUCT_TITLE } from "./product";

export interface AppInfo {
  title: string;
  name_zh: string;
  name_en: string;
  license: string;
  data_dir: string;
}

export async function syncProductIdentity(): Promise<AppInfo | null> {
  document.title = PRODUCT_TITLE;
  try {
    const info = await invoke<AppInfo>("app_info");
    if (info.title) document.title = info.title;
    return info;
  } catch {
    return null;
  }
}
