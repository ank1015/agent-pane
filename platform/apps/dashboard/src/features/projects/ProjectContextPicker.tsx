import { ArrowDown01Icon, Tick02Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type { IconSvgElement } from '@hugeicons/react'
import { memo, useId, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'

export type ProjectContextPickerOption = {
  id: string
  triggerLabel: string
  optionLabel: string
  searchValue?: string
  icon?: IconSvgElement
}

type ProjectContextPickerProps = {
  options: readonly ProjectContextPickerOption[]
  selectedOptionId: string | null
  ariaLabel: string
  listAriaLabel: string
  searchAriaLabel: string
  searchPlaceholder: string
  loadingLabel: string
  unavailableLabel: string
  emptyResultsLabel: string
  isPending: boolean
  disabled?: boolean
  showSelectedIcon?: boolean
  onSelect: (optionId: string) => void
}

export const ProjectContextPicker = memo(function ProjectContextPicker({
  options,
  selectedOptionId,
  ariaLabel,
  listAriaLabel,
  searchAriaLabel,
  searchPlaceholder,
  loadingLabel,
  unavailableLabel,
  emptyResultsLabel,
  isPending,
  disabled = false,
  showSelectedIcon = false,
  onSelect,
}: ProjectContextPickerProps) {
  const [isOpen, setIsOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [activeOptionId, setActiveOptionId] = useState<string | null>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const listboxId = useId()
  const selectedOption =
    options.find((option) => option.id === selectedOptionId) ?? null
  const normalizedQuery = query.trim().toLocaleLowerCase()
  const filteredOptions =
    normalizedQuery.length === 0
      ? options
      : options.filter((option) =>
          (option.searchValue ?? option.optionLabel)
            .toLocaleLowerCase()
            .includes(normalizedQuery),
        )
  const activeOptionIndex = filteredOptions.findIndex(
    (option) => option.id === activeOptionId,
  )

  const open = () => {
    setQuery('')
    setActiveOptionId(selectedOptionId)
    setIsOpen(true)
    requestAnimationFrame(() => searchRef.current?.focus())
  }

  const close = (restoreFocus = false) => {
    setIsOpen(false)
    setQuery('')
    setActiveOptionId(null)
    if (restoreFocus) {
      triggerRef.current?.focus()
    }
  }

  const select = (optionId: string) => {
    onSelect(optionId)
    close(true)
  }

  const moveActiveOption = (direction: 1 | -1) => {
    if (filteredOptions.length === 0) {
      return
    }

    const currentIndex = filteredOptions.findIndex(
      (option) => option.id === activeOptionId,
    )
    const nextIndex =
      currentIndex === -1
        ? direction === 1
          ? 0
          : filteredOptions.length - 1
        : (currentIndex + direction + filteredOptions.length) %
          filteredOptions.length
    setActiveOptionId(filteredOptions[nextIndex].id)
  }

  const handleMenuKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      moveActiveOption(event.key === 'ArrowDown' ? 1 : -1)
    } else if (event.key === 'Enter' && activeOptionId !== null) {
      const option = filteredOptions.find(
        (candidate) => candidate.id === activeOptionId,
      )
      if (option !== undefined) {
        event.preventDefault()
        select(option.id)
      }
    } else if (event.key === 'Escape') {
      event.preventDefault()
      close(true)
    }
  }

  const label =
    selectedOption?.triggerLabel ??
    (isPending ? loadingLabel : unavailableLabel)

  return (
    <div
      className={`project-context-picker${
        isOpen ? ' project-context-picker--open' : ''
      }`}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) {
          close()
        }
      }}
      onKeyDown={isOpen ? handleMenuKeyDown : undefined}
    >
      <button
        ref={triggerRef}
        type="button"
        className="project-context-picker-trigger"
        aria-label={`${ariaLabel}: ${label}`}
        aria-haspopup="listbox"
        aria-controls={isOpen ? listboxId : undefined}
        aria-expanded={isOpen}
        disabled={disabled || selectedOption === null}
        onClick={() => (isOpen ? close() : open())}
        onKeyDown={(event) => {
          if (
            !isOpen &&
            (event.key === 'ArrowDown' || event.key === 'ArrowUp')
          ) {
            event.preventDefault()
            open()
          }
        }}
      >
        {showSelectedIcon && selectedOption?.icon !== undefined ? (
          <HugeiconsIcon
            className="project-context-picker-trigger-icon"
            icon={selectedOption.icon}
            size={13}
            strokeWidth={1.5}
            aria-hidden="true"
          />
        ) : null}
        <span>{label}</span>
        <HugeiconsIcon
          icon={ArrowDown01Icon}
          size={11}
          strokeWidth={1.6}
          aria-hidden="true"
        />
      </button>

      {isOpen ? (
        <div className="project-context-picker-popover">
          <div className="project-context-picker-search-row">
            <input
              ref={searchRef}
              className="project-context-picker-search"
              value={query}
              placeholder={searchPlaceholder}
              aria-label={searchAriaLabel}
              aria-controls={listboxId}
              aria-activedescendant={
                activeOptionIndex === -1
                  ? undefined
                  : `${listboxId}-option-${activeOptionIndex}`
              }
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => {
                setQuery(event.target.value)
                setActiveOptionId(null)
              }}
            />
          </div>

          <div
            id={listboxId}
            className="project-context-picker-list"
            role="listbox"
            aria-label={listAriaLabel}
          >
            {filteredOptions.length === 0 ? (
              <p className="project-context-picker-empty">
                {emptyResultsLabel}
              </p>
            ) : (
              filteredOptions.map((option, optionIndex) => {
                const isSelected = option.id === selectedOptionId
                const isActive = option.id === activeOptionId

                return (
                  <button
                    key={option.id}
                    id={`${listboxId}-option-${optionIndex}`}
                    type="button"
                    className={`project-context-picker-option${
                      isActive ? ' project-context-picker-option--active' : ''
                    }`}
                    role="option"
                    aria-selected={isSelected}
                    onMouseEnter={() => setActiveOptionId(option.id)}
                    onMouseDown={(event) => event.preventDefault()}
                    disabled={disabled}
                    onClick={() => select(option.id)}
                  >
                    {option.icon !== undefined ? (
                      <HugeiconsIcon
                        className="project-context-picker-option-icon"
                        icon={option.icon}
                        size={16}
                        strokeWidth={1.5}
                        aria-hidden="true"
                      />
                    ) : null}
                    <span>{option.optionLabel}</span>
                    {isSelected ? (
                      <HugeiconsIcon
                        className="project-context-picker-option-check"
                        icon={Tick02Icon}
                        size={14}
                        strokeWidth={1.8}
                        aria-hidden="true"
                      />
                    ) : null}
                  </button>
                )
              })
            )}
          </div>
        </div>
      ) : null}
    </div>
  )
})
