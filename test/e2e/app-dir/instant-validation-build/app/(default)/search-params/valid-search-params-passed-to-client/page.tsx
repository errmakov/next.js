import type { Instant } from 'next'
import assert from 'node:assert/strict'

import { ClientChild } from './client'
import { Suspense, Fragment } from 'react'

export const instant: Instant = {
  level: 'experimental-error',
  unstable_samples: [
    {
      searchParams: {
        single: 'test',
        multiple: ['a', 'b'],
      },
    },
  ],
}
export const prefetch = 'allow-runtime'

export default async function Page({
  searchParams,
}: {
  searchParams: Promise<Record<string, string | string[]>>
}) {
  return (
    <main>
      <SuspenseInAppShells>
        <TestSearchParams searchParams={searchParams} />
      </SuspenseInAppShells>
    </main>
  )
}

const SuspenseInAppShells = process.env.__NEXT_APP_SHELLS ? Suspense : Fragment

async function TestSearchParams({
  searchParams,
}: {
  searchParams: Promise<Record<string, string | string[]>>
}) {
  const sp = await searchParams
  assert.equal(
    sp.single,
    'test',
    `Expected 'single' to be 'test', got '${sp.single}'`
  )
  assert.deepStrictEqual(
    sp.multiple,
    ['a', 'b'],
    `Unexpected value for 'multiple'`
  )
  return <ClientChild searchParams={sp} />
}
