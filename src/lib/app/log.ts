import {
  error as pluginError,
  info as pluginInfo,
  warn as pluginWarn,
} from "@tauri-apps/plugin-log";

export function formatLogError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

export async function appLogInfo(message: string) {
  try {
    await pluginInfo(message);
  } catch (err) {
    console.info(message, err);
  }
}

export async function appLogWarn(message: string) {
  try {
    await pluginWarn(message);
  } catch (err) {
    console.warn(message, err);
  }
}

export async function appLogError(message: string) {
  try {
    await pluginError(message);
  } catch (err) {
    console.error(message, err);
  }
}
