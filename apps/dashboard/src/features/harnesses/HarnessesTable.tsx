import { type HarnessSummary, useHarnesses } from './harness-queries'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})

export function HarnessesTable() {
  const query = useHarnesses()

  return (
    <section
      className="harnesses-section"
      aria-labelledby="configured-harnesses-title"
    >
      <div className="harnesses-section-header">
        <h2 id="configured-harnesses-title">Configured harnesses</h2>
      </div>

      <div className="providers-table-wrap">
        <table className="providers-table harnesses-table">
          <colgroup>
            <col className="harness-name-column" />
            <col className="harness-description-column" />
            <col className="harness-created-column" />
            <col className="harness-updated-column" />
          </colgroup>
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Description</th>
              <th scope="col">Created at</th>
              <th scope="col">Updated at</th>
            </tr>
          </thead>
          <tbody aria-live="polite" aria-busy={query.isPending}>
            <HarnessRows query={query} />
          </tbody>
        </table>
      </div>
    </section>
  )
}

function HarnessRows({ query }: { query: ReturnType<typeof useHarnesses> }) {
  if (query.isPending) {
    return <HarnessTableMessage message="Loading harnesses…" />
  }
  if (query.isError) {
    return (
      <tr>
        <td
          className="providers-table-message"
          colSpan={4}
          title={query.error.message}
        >
          Couldn&apos;t load harnesses
          <button
            type="button"
            className="providers-retry-button"
            onClick={() => void query.refetch()}
          >
            Retry
          </button>
        </td>
      </tr>
    )
  }
  if (query.data.length === 0) {
    return <HarnessTableMessage message="No harnesses configured" />
  }

  return query.data.map((harness) => (
    <HarnessRow key={harness.id} harness={harness} />
  ))
}

function HarnessRow({ harness }: { harness: HarnessSummary }) {
  return (
    <tr>
      <td>
        <span className="harness-name" title={harness.name}>
          {harness.name}
        </span>
      </td>
      <td>
        <span
          className="harness-detail harness-description"
          title={harness.description ?? undefined}
        >
          {harness.description ?? '—'}
        </span>
      </td>
      <td>
        <HarnessDate value={harness.created_at} />
      </td>
      <td>
        <HarnessDate value={harness.updated_at} />
      </td>
    </tr>
  )
}

function HarnessDate({ value }: { value: string }) {
  return (
    <time className="harness-detail" dateTime={value} title={value}>
      {formatDate(value)}
    </time>
  )
}

function HarnessTableMessage({ message }: { message: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={4}>
        {message}
      </td>
    </tr>
  )
}

function formatDate(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : DATE_FORMATTER.format(date)
}
