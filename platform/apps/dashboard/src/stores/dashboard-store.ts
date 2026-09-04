import { create } from 'zustand'
import { persist } from 'zustand/middleware'

type DashboardState = {
  isSidebarCollapsed: boolean
  collapseSidebar: () => void
  expandSidebar: () => void
  toggleSidebar: () => void
}

export const useDashboardStore = create<DashboardState>()(
  persist(
    (set) => ({
      isSidebarCollapsed: false,
      collapseSidebar: () => set({ isSidebarCollapsed: true }),
      expandSidebar: () => set({ isSidebarCollapsed: false }),
      toggleSidebar: () =>
        set((state) => ({
          isSidebarCollapsed: !state.isSidebarCollapsed,
        })),
    }),
    {
      name: 'agent-pane-platform-dashboard',
      partialize: (state) => ({
        isSidebarCollapsed: state.isSidebarCollapsed,
      }),
    },
  ),
)
