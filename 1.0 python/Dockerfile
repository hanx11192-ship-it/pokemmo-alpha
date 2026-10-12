FROM python:3.11-slim

WORKDIR /app

COPY requirements-panel.txt .
RUN pip install --no-cache-dir -r requirements-panel.txt

COPY . .

EXPOSE 5000
ENV PORT=5000
ENV PANEL_SECRET=change-me-in-prod

CMD ["python", "panel/app.py"]
