import { CloudIcon, ComputerIcon } from '@hugeicons/core-free-icons'
import { memo, useMemo } from 'react'
import { ProjectContextPicker } from './ProjectContextPicker'
import type { ProjectContextPickerOption } from './ProjectContextPicker'
import type { ProjectEnvironment } from './project-queries'

type ProjectEnvironmentSelectorProps = {
  environments: readonly ProjectEnvironment[]
  selectedEnvironmentId: string | null
  isPending: boolean
  onSelect: (environmentId: string) => void
}

export const ProjectEnvironmentSelector = memo(
  function ProjectEnvironmentSelector({
    environments,
    selectedEnvironmentId,
    isPending,
    onSelect,
  }: ProjectEnvironmentSelectorProps) {
    const options = useMemo<readonly ProjectContextPickerOption[]>(
      () =>
        environments.map((environment) => ({
          id: environment.id,
          triggerLabel: environment.name,
          optionLabel: environment.name,
          searchValue: `${environment.name} ${environment.host_name} ${environment.path}`,
          icon: environment.type === 'env' ? ComputerIcon : CloudIcon,
        })),
      [environments],
    )

    return (
      <ProjectContextPicker
        options={options}
        selectedOptionId={selectedEnvironmentId}
        ariaLabel="Environment"
        listAriaLabel="Project environments"
        searchAriaLabel="Search environments"
        searchPlaceholder="Search environments..."
        loadingLabel="Loading environments…"
        unavailableLabel="No environments available"
        emptyResultsLabel="No environments found"
        isPending={isPending}
        showSelectedIcon
        onSelect={onSelect}
      />
    )
  },
)
