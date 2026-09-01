export type HarnessEnvironmentSelectionMode = 'none' | 'single' | 'multiple'

export type HarnessUiDefinition = {
  environmentSelection: HarnessEnvironmentSelectionMode
}

const DEFAULT_HARNESS_UI: HarnessUiDefinition = {
  environmentSelection: 'none',
}

const HARNESS_UI_DEFINITIONS: Readonly<Record<string, HarnessUiDefinition>> = {
  codex: { environmentSelection: 'single' },
  environment: { environmentSelection: 'none' },
  pi: { environmentSelection: 'single' },
}

export function harnessUiDefinition(
  harnessId: string | null,
): HarnessUiDefinition {
  return harnessId === null
    ? DEFAULT_HARNESS_UI
    : (HARNESS_UI_DEFINITIONS[harnessId] ?? DEFAULT_HARNESS_UI)
}
