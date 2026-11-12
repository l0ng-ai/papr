import { invoke } from "@tauri-apps/api/core";

/** Choose a destination in Rust and save UTF-8 text. Cancellation is not success. */
export function saveTextFile(
  content: string,
  defaultName: string,
  filterName: string,
  extensions: string[],
): Promise<boolean> {
  return invoke<boolean>("save_text_file", { content, defaultName, filterName, extensions });
}
