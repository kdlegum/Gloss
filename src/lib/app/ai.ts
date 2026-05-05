import type {
  AiTask,
  AiTaskSettings,
  AnyProvider,
  ChatProvider,
  ChunkingProvider,
  ModelOption,
  VisionProvider,
} from "$lib/app/types";

export const LEGACY_CHUNKING_PROVIDER_STORAGE_KEY = "gloss_chunking_provider";
export const AI_TASK_SETTINGS_STORAGE_KEY = "gloss_ai_task_settings_v1";
export const CUSTOM_MODEL_VALUE = "__custom__";

export const CHUNKING_PROVIDER_OPTIONS: Array<{ value: ChunkingProvider; label: string; short: string }> = [
  { value: "ollama", label: "Ollama", short: "OL" },
  { value: "openai", label: "OpenAI", short: "OA" },
  { value: "gemini", label: "Gemini", short: "GM" },
  { value: "deepseek", label: "DeepSeek", short: "DS" },
];

export const CHAT_PROVIDER_OPTIONS: Array<{ value: ChatProvider; label: string; short: string }> = [
  { value: "ollama", label: "Ollama", short: "OL" },
  { value: "openai", label: "OpenAI", short: "OA" },
  { value: "gemini", label: "Gemini", short: "GM" },
  { value: "deepseek", label: "DeepSeek", short: "DS" },
];

export const VISION_PROVIDER_OPTIONS: Array<{ value: VisionProvider; label: string; short: string }> = [
  { value: "ollama", label: "Ollama", short: "OL" },
  { value: "openai", label: "OpenAI", short: "OA" },
  { value: "gemini", label: "Gemini", short: "GM" },
  { value: "zai", label: "Z.AI", short: "ZA" },
];

export const CHUNKING_MODEL_OPTIONS: Record<ChunkingProvider, ModelOption[]> = {
  ollama: [
    { value: "", label: "Auto (server default)" },
  ],
  openai: [
    { value: "gpt-5.4-mini", label: "gpt-5.4-mini" },
    { value: "gpt-5.4", label: "gpt-5.4" },
    { value: "gpt-5-nano", label: "gpt-5-nano" },
  ],
  gemini: [
    { value: "gemini-2.5-flash", label: "gemini-2.5-flash" },
    { value: "gemini-2.5-flash-lite", label: "gemini-2.5-flash-lite" },
  ],
  deepseek: [
    { value: "deepseek-v4-flash", label: "deepseek-v4-flash" },
    { value: "deepseek-v4-pro", label: "deepseek-v4-pro" },
    { value: "deepseek-reasoner", label: "deepseek-reasoner", legacy: true },
  ],
};

export const CHAT_MODEL_OPTIONS: Record<ChatProvider, ModelOption[]> = {
  ollama: [
    { value: "", label: "Auto (server default)" },
  ],
  openai: [
    { value: "gpt-5.4-mini", label: "gpt-5.4-mini" },
    { value: "gpt-5.4", label: "gpt-5.4" },
    { value: "gpt-5-nano", label: "gpt-5-nano" },
  ],
  gemini: [
    { value: "gemini-2.5-flash", label: "gemini-2.5-flash" },
    { value: "gemini-2.5-flash-lite", label: "gemini-2.5-flash-lite" },
  ],
  deepseek: [
    { value: "deepseek-v4-flash", label: "deepseek-v4-flash" },
    { value: "deepseek-v4-pro", label: "deepseek-v4-pro" },
    { value: "deepseek-reasoner", label: "deepseek-reasoner", legacy: true },
  ],
};

export const VISION_MODEL_OPTIONS: Record<VisionProvider, ModelOption[]> = {
  ollama: [
    { value: "", label: "Auto (server default)" },
  ],
  openai: [
    { value: "gpt-5.4-mini", label: "gpt-5.4-mini" },
    { value: "gpt-5.4", label: "gpt-5.4" },
    { value: "gpt-5-nano", label: "gpt-5-nano" },
  ],
  gemini: [
    { value: "gemini-2.5-flash", label: "gemini-2.5-flash" },
    { value: "gemini-2.5-flash-lite", label: "gemini-2.5-flash-lite" },
  ],
  zai: [
    { value: "glm-ocr", label: "glm-ocr" },
  ],
};

export function isChunkingProvider(value: string | null): value is ChunkingProvider {
  return value === "ollama" || value === "openai" || value === "gemini" || value === "deepseek";
}

export function isChatProvider(value: string | null): value is ChatProvider {
  return value === "ollama" || value === "openai" || value === "gemini" || value === "deepseek";
}

export function isVisionProvider(value: string | null): value is VisionProvider {
  return value === "ollama" || value === "openai" || value === "gemini" || value === "zai";
}

export function getProviderLabel(provider: AnyProvider): string {
  switch (provider) {
    case "ollama":
      return "Ollama";
    case "openai":
      return "OpenAI";
    case "gemini":
      return "Gemini";
    case "deepseek":
      return "DeepSeek";
    case "zai":
      return "Z.AI";
  }
}

export function getChunkingProviderLabel(provider: ChunkingProvider) {
  return getProviderLabel(provider);
}

export function getDefaultModelForTask(task: AiTask, provider: AnyProvider): string {
  if (task === "chunking") {
    const model = CHUNKING_MODEL_OPTIONS[provider as ChunkingProvider]?.[0]?.value;
    return model ?? "";
  }
  if (task === "chat") {
    const model = CHAT_MODEL_OPTIONS[provider as ChatProvider]?.[0]?.value;
    return model ?? "";
  }
  const model = VISION_MODEL_OPTIONS[provider as VisionProvider]?.[0]?.value;
  return model ?? "";
}

export function defaultAiTaskSettings(): AiTaskSettings {
  return {
    chunking: { provider: "deepseek", model: "deepseek-v4-flash" },
    chat: { provider: "gemini", model: "gemini-2.5-flash" },
    vision: { provider: "zai", model: "glm-ocr" },
  };
}

export function getModelOptions(task: AiTask, provider: AnyProvider): ModelOption[] {
  if (task === "chunking") return CHUNKING_MODEL_OPTIONS[provider as ChunkingProvider] ?? [];
  if (task === "chat") return CHAT_MODEL_OPTIONS[provider as ChatProvider] ?? [];
  return VISION_MODEL_OPTIONS[provider as VisionProvider] ?? [];
}

export function isModelInOptions(task: AiTask, provider: AnyProvider, model: string): boolean {
  return getModelOptions(task, provider).some((option) => option.value === model);
}
