import { ProvidersTable } from './ProvidersTable'

export function ProvidersPage() {
  return (
    <div className="providers-page">
      <header className="providers-page-header">
        <h1 className="cursor-page-title">Providers</h1>
      </header>
      <ProvidersTable />
    </div>
  )
}
