import js from '@eslint/js'
import tseslint from 'typescript-eslint'
import pluginVue from 'eslint-plugin-vue'

export default [
  {
    ignores: [
      '.local/**',
      '.design-review/**',
      'dist',
      'src-tauri/target',
      'node_modules',
      '*.config.ts',
      '*.config.js',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...pluginVue.configs['flat/recommended'],
  {
    files: ['**/*.vue'],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
      },
    },
  },
  {
    // 纯 Node 脚本没有 @types/node 兜底，显式声明用到的少量全局
    files: ['scripts/**/*.mjs'],
    languageOptions: {
      globals: {
        process: 'readonly',
        console: 'readonly',
      },
    },
  },
  {
    rules: {
      '@typescript-eslint/no-explicit-any': 'warn',
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],
      // Prettier owns formatting (pnpm fmt); these vue layout rules conflict
      // with it on short elements. Official eslint-plugin-vue guidance is to
      // turn them off when a separate formatter is used.
      'vue/max-attributes-per-line': 'off',
      'vue/singleline-html-element-content-newline': 'off',
      'vue/html-self-closing': 'off',
      'vue/html-closing-bracket-newline': 'off',
      'vue/html-indent': 'off',
      // These are deliberately named after familiar native controls. They are
      // base design-system primitives rather than route components.
      'vue/multi-word-component-names': ['error', { ignores: ['Button', 'Toaster', 'Tooltip'] }],
    },
  },
]
