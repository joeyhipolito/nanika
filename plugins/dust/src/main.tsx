import React from 'react'
import ReactDOM from 'react-dom/client'
import './globals.css'

const useLegacy =
  import.meta.env.VITE_DUST_LEGACY_LAUNCHER === '1' ||
  import.meta.env.VITE_DUST_LEGACY_LAUNCHER === 'true'

const loader = useLegacy
  ? import('./App').then((m) => m.App)
  : import('./whim/App').then((m) => m.App)

loader.then((App) => {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  )
})
