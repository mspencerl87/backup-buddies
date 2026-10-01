FROM node:20-slim

WORKDIR /app

# Install deps first so this layer is cached across code-only changes
COPY package.json package-lock.json* ./
RUN npm install --omit=dev

COPY src ./src

ENV NODE_ENV=production
EXPOSE 3000

CMD ["node", "src/index.js"]
