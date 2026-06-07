import { useEffect, useRef } from 'react'

/**
 * Subscribe to a Server-Sent Events stream **via fetch** (not `EventSource`)
 * so we can send the `Authorization` header — EventSource can't. Parses
 * `event:`/`data:` frames and invokes `onEvent(kind, data)` per message.
 * Aborts on unmount / dependency change.
 */
export function useSSE(
  url: string,
  token: string,
  enabled: boolean,
  onEvent: (kind: string, data: string) => void,
) {
  const cb = useRef(onEvent)
  cb.current = onEvent

  useEffect(() => {
    if (!enabled) return
    const ctrl = new AbortController()

    void (async () => {
      try {
        const res = await fetch(url, {
          headers: { authorization: `Bearer ${token}` },
          signal: ctrl.signal,
        })
        if (!res.ok || !res.body) return
        const reader = res.body.getReader()
        const decoder = new TextDecoder()
        let buf = ''
        let evName = 'message'
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          buf += decoder.decode(value, { stream: true })
          let nl: number
          while ((nl = buf.indexOf('\n')) >= 0) {
            const line = buf.slice(0, nl).replace(/\r$/, '')
            buf = buf.slice(nl + 1)
            if (line.startsWith('event:')) evName = line.slice(6).trim()
            else if (line.startsWith('data:')) cb.current(evName, line.slice(5).trim())
            else if (line === '') evName = 'message'
          }
        }
      } catch {
        /* aborted or transient network error */
      }
    })()

    return () => ctrl.abort()
  }, [url, token, enabled])
}
