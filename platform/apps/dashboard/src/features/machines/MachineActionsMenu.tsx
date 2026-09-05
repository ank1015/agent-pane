import { RenameDeleteMenu } from '../../components/RenameDeleteMenu'

export function MachineActionsMenu({ name, onAction }: { name: string; onAction: (action: 'rename' | 'delete', trigger: HTMLButtonElement) => void }) {
  return <RenameDeleteMenu name={name} onAction={onAction} triggerClassName="machine-card-actions" />
}
