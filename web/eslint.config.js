// Strictest typescript-eslint presets, type-aware.
import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist/", "node_modules/", "eslint.config.js"] },
  js.configs.recommended,
  tseslint.configs.strictTypeChecked,
  tseslint.configs.stylisticTypeChecked,
  {
    languageOptions: { parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname } },
    rules: {
      eqeqeq: "error",
      "no-console": "error",
      "prefer-const": "error",
      "@typescript-eslint/explicit-function-return-type": ["error", { allowExpressions: true }],
      "@typescript-eslint/switch-exhaustiveness-check": "error",
      // Values are typed by decoding or construction, never by assertion.
      "@typescript-eslint/consistent-type-assertions": ["error", { assertionStyle: "never" }],
      "@typescript-eslint/no-explicit-any": "error",
      "@typescript-eslint/no-restricted-types": [
        "error",
        { types: { unknown: "Decode JSON with decode.ts; nothing else is untyped." } },
      ],
    },
  },
  // The one place unchecked input (JSON.parse) is looked at.
  { files: ["decode.ts"], rules: { "@typescript-eslint/no-restricted-types": "off" } },
  // Tests run in Bun: type-checked against its types, not the browser's.
  {
    files: ["*.test.ts"],
    languageOptions: { parserOptions: { projectService: false, project: "./tsconfig.test.json" } },
  },
);
