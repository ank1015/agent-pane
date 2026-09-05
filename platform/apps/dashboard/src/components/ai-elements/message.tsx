import { cn } from '@/lib/utils'
import { cjk } from '@streamdown/cjk'
import { code } from '@streamdown/code'
import { CheckIcon, CopyIcon, TextWrapIcon } from 'lucide-react'
import type { UIMessage } from 'ai'
import type { ComponentProps, HTMLAttributes } from 'react'
import { memo, useState } from 'react'
import {
  CodeBlock,
  CodeBlockCopyButton,
  Streamdown,
  type ExtraProps,
  useIsCodeFenceIncomplete,
} from 'streamdown'

export type MessageProps = HTMLAttributes<HTMLDivElement> & {
  from: UIMessage['role']
}

export function Message({ className, from, ...props }: MessageProps) {
  return (
    <div
      className={cn(
        'group flex w-full max-w-[95%] flex-col gap-2',
        from === 'user' ? 'is-user ml-auto justify-end' : 'is-assistant',
        className,
      )}
      {...props}
    />
  )
}

export type MessageContentProps = HTMLAttributes<HTMLDivElement>

export function MessageContent({
  children,
  className,
  ...props
}: MessageContentProps) {
  return (
    <div
      className={cn(
        'flex w-fit min-w-0 max-w-full flex-col gap-2 overflow-hidden text-sm',
        'group-[.is-user]:ml-auto group-[.is-user]:rounded-lg group-[.is-user]:bg-secondary group-[.is-user]:px-4 group-[.is-user]:py-3 group-[.is-user]:text-foreground',
        'group-[.is-assistant]:text-foreground',
        className,
      )}
      {...props}
    >
      {children}
    </div>
  )
}

export type MessageResponseProps = ComponentProps<typeof Streamdown>

const streamdownPlugins = { cjk, code }
const codeThemes: NonNullable<MessageResponseProps['shikiTheme']> = [
  'one-dark-pro',
  'one-dark-pro',
]
const streamdownIcons: NonNullable<MessageResponseProps['icons']> = {
  CheckIcon,
  CopyIcon,
}

type MarkdownCodeProps = ComponentProps<'code'> &
  ExtraProps & {
    'data-block'?: string
  }

type MarkdownTableProps = ComponentProps<'table'> & ExtraProps

function MarkdownCode({
  children,
  className,
  node: _node,
  ...props
}: MarkdownCodeProps) {
  const isIncomplete = useIsCodeFenceIncomplete()
  const { 'data-block': dataBlock, ...codeProps } = props

  if (dataBlock === undefined) {
    return (
      <code
        className={className}
        data-streamdown="inline-code"
        {...codeProps}
      >
        {children}
      </code>
    )
  }

  const language = className?.match(/language-([\w-]+)/)?.[1] ?? 'text'
  const rawCode = Array.isArray(children)
    ? children.join('')
    : String(children ?? '')
  const codeText = rawCode.replace(/\n$/, '')

  return (
    <MarkdownCodeBlock
      code={codeText}
      isIncomplete={isIncomplete}
      language={language}
    />
  )
}

function MarkdownCodeBlock({
  code: codeText,
  isIncomplete,
  language,
}: {
  code: string
  isIncomplete: boolean
  language: string
}) {
  const [isWrapped, setIsWrapped] = useState(false)

  return (
    <CodeBlock
      className={isWrapped ? 'project-session-code-is-wrapped' : undefined}
      code={codeText}
      isIncomplete={isIncomplete}
      language={language}
      lineNumbers={false}
    >
      <button
        type="button"
        className="project-session-code-control"
        aria-label={isWrapped ? 'Disable code wrapping' : 'Wrap code'}
        aria-pressed={isWrapped}
        title={isWrapped ? 'Disable code wrapping' : 'Wrap code'}
        onClick={() => setIsWrapped((wrapped) => !wrapped)}
      >
        <TextWrapIcon aria-hidden="true" size={13} />
      </button>
      <CodeBlockCopyButton
        className="project-session-code-control"
        timeout={1600}
      />
    </CodeBlock>
  )
}

function MarkdownTable({
  children,
  className,
  node: _node,
  ...props
}: MarkdownTableProps) {
  return (
    <div className="project-session-table-wrap">
      <table className={cn('project-session-table', className)} {...props}>
        {children}
      </table>
    </div>
  )
}

const markdownComponents: NonNullable<MessageResponseProps['components']> = {
  code: MarkdownCode,
  table: MarkdownTable,
}

export const MessageResponse = memo(
  ({ className, ...props }: MessageResponseProps) => (
    <Streamdown
      className={cn(
        'size-full [&>*:first-child]:mt-0 [&>*:last-child]:mb-0',
        className,
      )}
      components={markdownComponents}
      icons={streamdownIcons}
      lineNumbers={false}
      plugins={streamdownPlugins}
      shikiTheme={codeThemes}
      {...props}
    />
  ),
  (previous, next) =>
    previous.children === next.children &&
    previous.isAnimating === next.isAnimating,
)

MessageResponse.displayName = 'MessageResponse'
