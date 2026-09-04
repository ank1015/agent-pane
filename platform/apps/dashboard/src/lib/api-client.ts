export class ApiError extends Error {
  readonly status: number
  readonly body: unknown

  constructor(status: number, message: string, body: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.body = body
  }
}

export async function getJson<T>(
  path: string,
  signal?: AbortSignal,
): Promise<T> {
  const response = await fetch(path, {
    headers: { Accept: 'application/json' },
    signal,
  })

  return parseResponse<T>(response)
}

export async function postJson<T>(path: string, body: unknown): Promise<T> {
  const response = await fetch(path, {
    method: 'POST',
    headers: { Accept: 'application/json', 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  return parseResponse<T>(response)
}

export async function patchJson<T>(path: string, body: unknown): Promise<T> {
  const response = await fetch(path, {
    method: 'PATCH',
    headers: { Accept: 'application/json', 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  return parseResponse<T>(response)
}

async function parseResponse<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const body = await readJson(response)
    throw new ApiError(response.status, errorMessage(body), body)
  }

  return response.json() as Promise<T>
}

export async function deleteRequest(path: string): Promise<void> {
  const response = await fetch(path, { method: 'DELETE', headers: { Accept: 'application/json' } })
  if (!response.ok) {
    const body = await readJson(response)
    throw new ApiError(response.status, errorMessage(body), body)
  }
}

async function readJson(response: Response): Promise<unknown> {
  try {
    return await response.json()
  } catch {
    return null
  }
}

function errorMessage(body: unknown): string {
  if (
    typeof body === 'object' &&
    body !== null &&
    'error' in body &&
    typeof body.error === 'object' &&
    body.error !== null &&
    'message' in body.error &&
    typeof body.error.message === 'string'
  ) {
    return body.error.message
  }

  return 'The request could not be completed.'
}
