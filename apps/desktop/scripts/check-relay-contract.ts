import {
  validateRelayApiV1Contract,
  relayApiV1ContractDirectory,
} from './relay-contract-lib.ts'

async function main(): Promise<void> {
  const validated = await validateRelayApiV1Contract()
  console.log(
    `Relay HTTP v1 contract OK: ${validated.manifest.contract} (${validated.manifest.fixtures.length} fixtures, manifest sha256 ${validated.manifestSha256})`,
  )
  console.log(`Directory: ${relayApiV1ContractDirectory}`)
}

main().catch((error: unknown) => {
  console.error(`ERROR ${error instanceof Error ? error.message : String(error)}`)
  process.exitCode = 1
})
