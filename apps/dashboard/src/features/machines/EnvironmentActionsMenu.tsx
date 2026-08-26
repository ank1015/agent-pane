import { AtIcon, Delete03Icon } from '@hugeicons/core-free-icons'
import { TableActionsMenu } from '../../components/TableActionsMenu'
import type { MachineEnvironment } from './machine-queries'

type EnvironmentActionsMenuProps = {
  environment: MachineEnvironment
  onDelete: () => void
  onUpdateName: () => void
}

export function EnvironmentActionsMenu({
  environment,
  onDelete,
  onUpdateName,
}: EnvironmentActionsMenuProps) {
  return (
    <TableActionsMenu
      label={`Actions for ${environment.name}`}
      actions={[
        { icon: AtIcon, label: 'Update name', onSelect: onUpdateName },
        {
          danger: true,
          icon: Delete03Icon,
          label: 'Delete',
          onSelect: onDelete,
          separatorBefore: true,
        },
      ]}
    />
  )
}
