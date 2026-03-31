import { createContext, useContext } from 'react'

export interface DashboardState {
  activeModuleId: string | null
  previousModuleId: string | null
  openModule: (id: string) => void
}

export const DashboardStateContext = createContext<DashboardState>({
  activeModuleId: null,
  previousModuleId: null,
  openModule: () => {},
})

export function useDashboardState(): DashboardState {
  return useContext(DashboardStateContext)
}
