import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const channelPort = env.DASHBOARD_CHANNEL_PORT ?? '7332'
  const channelTarget = `http://localhost:${channelPort}`
  return {
    plugins: [tailwindcss(), react()],
    resolve: {
      alias: {
        "@": path.resolve(__dirname, "./src"),
      },
    },
    server: {
      proxy: {
        '/api/orchestrator': {
          target: 'http://localhost:7331',
          changeOrigin: true,
          rewrite: (path) => path.replace('/api/orchestrator', '/api'),
        },
        '/api/channel': {
          target: channelTarget,
          changeOrigin: true,
          rewrite: (path) => path.replace('/api/channel', '/channel'),
        },
        '/api/sse': {
          target: channelTarget,
          changeOrigin: true,
          rewrite: (path) => path.replace('/api/sse', '/events'),
        },
        '/api/stt/ws': {
          target: 'wss://api.elevenlabs.io',
          ws: true,
          changeOrigin: true,
          rewrite: (path) => path.replace('/api/stt/ws', '/v1/speech-to-text/realtime'),
          headers: {
            'xi-api-key': env.ELEVENLABS_API_KEY || '',
          },
        },
      },
    },
  }
})
