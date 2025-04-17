module.exports = {
  content: [
    './pages/**/*.{js,ts,jsx,tsx,mdx}',
    './components/**/*.{js,ts,jsx,tsx,mdx}',
    './app/**/*.{js,ts,jsx,tsx,mdx}',
  ],
  theme: {
    extend: {},
  },
  plugins: [require("daisyui")],
  daisyui: {
    themes: [
      "dark", "light", "cupcake", "bumblebee", "emerald", "corporate", "synthwave", 
      "retro", "cyberpunk", "valentine", "halloween", "garden", "forest", "aqua", 
      "lofi", "pastel", "fantasy", "wireframe", "black", "luxury", "dracula", "cmyk", 
      "autumn", "business", "acid", "lemonade", "night", "coffee", "winter", "dim", "nord", "sunset"
    ],
  },
}