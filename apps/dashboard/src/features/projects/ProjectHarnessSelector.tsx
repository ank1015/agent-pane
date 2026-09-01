import { memo, useMemo } from 'react'
import { ProjectContextPicker } from './ProjectContextPicker'
import type { ProjectContextPickerOption } from './ProjectContextPicker'
import type { ProjectBootstrapHarness } from './project-queries'

type ProjectHarnessSelectorProps = {
  harnesses: readonly ProjectBootstrapHarness[]
  selectedHarnessId: string | null
  isPending: boolean
  onSelect: (harnessId: string) => void
}

export const ProjectHarnessSelector = memo(function ProjectHarnessSelector({
  harnesses,
  selectedHarnessId,
  isPending,
  onSelect,
}: ProjectHarnessSelectorProps) {
  const options = useMemo<readonly ProjectContextPickerOption[]>(
    () =>
      harnesses.map((harness) => ({
        id: harness.harness_id,
        triggerLabel: harness.harness_id,
        optionLabel: `${harness.harness_id} harness`,
      })),
    [harnesses],
  )

  return (
    <ProjectContextPicker
      options={options}
      selectedOptionId={selectedHarnessId}
      ariaLabel="Harness"
      listAriaLabel="Harnesses"
      searchAriaLabel="Search harnesses"
      searchPlaceholder="Search harnesses..."
      loadingLabel="Loading harnesses…"
      unavailableLabel="No harnesses available"
      emptyResultsLabel="No harnesses found"
      isPending={isPending}
      onSelect={onSelect}
    />
  )
})
