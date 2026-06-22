import { connection } from 'next/server'
import { Suspense, Fragment } from 'react'

export const instant = {
  level: 'experimental-error',
  unstable_samples: [{ cookies: [], params: { param: '123' } }],
}
export const prefetch = 'allow-runtime'

export default async function Page({
  params,
}: {
  params: Promise<{ param: string }>
}) {
  return (
    <main>
      <div>
        <p>Params don't need a suspense boundary when runtime-prefetched:</p>
        <SuspenseInAppShells>
          <LinkData params={params} />
        </SuspenseInAppShells>
      </div>

      <div>
        <p>But dynamic content does:</p>
        <Suspense fallback={<div>Loading...</div>}>
          <Dynamic />
        </Suspense>
      </div>
    </main>
  )
}

async function LinkData({ params }: { params: Promise<{ param: string }> }) {
  const { param } = await params
  return <div id="runtime-content">Param value: {param}</div>
}

const SuspenseInAppShells = process.env.__NEXT_APP_SHELLS ? Suspense : Fragment

async function Dynamic() {
  await connection()
  return <div id="dynamic-content">Dynamic content from page</div>
}
