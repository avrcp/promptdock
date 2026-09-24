/**
 * Generated package configuration. Keep dependency versions exact and treat
 * `src/generated` as disposable output.
 * @type {import('@hey-api/openapi-ts').UserConfig}
 */
export default {
  input: '../../contracts/admin-api/v2/openapi.json',
  output: {
    path: './src/generated',
    clean: true,
  },
  plugins: [
    '@hey-api/client-fetch',
    { name: '@hey-api/typescript', enums: 'javascript' },
    { name: '@hey-api/sdk', validator: 'zod' },
    {
      name: 'zod',
      compatibilityVersion: 3,
      requests: true,
      responses: true,
    },
  ],
}
