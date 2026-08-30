import { useCreateMachineEnvironment } from './machine-queries'
import {
  type EnvironmentDraft,
  EnvironmentDialogFlow,
} from './EnvironmentDialogFlow'

type AddEnvironmentDialogProps = {
  machineId: string
  open: boolean
  workspaceRootId: string
  workspaceRootPath: string
  onClose: () => void
}

export function AddEnvironmentDialog({
  machineId,
  open,
  workspaceRootId,
  workspaceRootPath,
  onClose,
}: AddEnvironmentDialogProps) {
  const createEnvironment = useCreateMachineEnvironment()

  const create = ({ name, path }: EnvironmentDraft) =>
    createEnvironment.mutateAsync({
      machineId,
      name,
      workspaceRootId,
      path,
    })

  return (
    <EnvironmentDialogFlow
      canFinish={workspaceRootId.length > 0}
      error={
        createEnvironment.isError ? createEnvironment.error.message : null
      }
      isPending={createEnvironment.isPending}
      open={open}
      workspaceRootPath={workspaceRootPath}
      onClose={onClose}
      onFinish={create}
      onReset={createEnvironment.reset}
    />
  )
}
