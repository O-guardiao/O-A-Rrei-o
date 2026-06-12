use anyhow::Result;

/// Gera o conteúdo do Dockerfile para build e runtime do Arreio.
pub fn generate_dockerfile() -> String {
    r#"# Build stage
FROM rust:1.82-slim-bookworm AS builder
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y libsqlite3-dev pkg-config
RUN cargo build --release --bin arreio-cli

# Runtime stage
FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y libsqlite3-0 ca-certificates
COPY --from=builder /app/target/release/arreio-cli /usr/local/bin/arreio
COPY --from=builder /app/crates/arreio-gateway/assets /app/assets
EXPOSE 8080
ENTRYPOINT ["arreio"]
CMD ["serve"]
"#
    .to_string()
}

/// Gera o conteúdo do docker-compose.yml com os serviços arreio e ollama.
pub fn generate_docker_compose() -> String {
    r#"version: '3.8'
services:
  arreio:
    build: .
    ports:
      - "8080:8080"
    environment:
      - OPENAI_API_KEY=${OPENAI_API_KEY}
      - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
      - GOOGLE_API_KEY=${GOOGLE_API_KEY}
    volumes:
      - arreio-data:/app/data
      - ./workspace:/workspace
    working_dir: /workspace
    command: ["serve", "--port", "8080"]
    
  ollama:
    image: ollama/ollama:latest
    ports:
      - "11434:11434"
    volumes:
      - ollama-models:/root/.ollama
      
volumes:
  arreio-data:
  ollama-models:
"#
    .to_string()
}

/// Gera o conteúdo do .dockerignore para o projeto.
pub fn generate_dockerignore() -> String {
    r#"target/
.git/
*.md
!CLAUDE.md
!AGENTS.md
"#
    .to_string()
}

/// Escreve o Dockerfile no caminho especificado.
pub fn write_dockerfile(path: &std::path::Path) -> Result<()> {
    std::fs::write(path, generate_dockerfile())?;
    Ok(())
}

/// Escreve o docker-compose.yml no caminho especificado.
pub fn write_docker_compose(path: &std::path::Path) -> Result<()> {
    std::fs::write(path, generate_docker_compose())?;
    Ok(())
}

/// Escreve o .dockerignore no caminho especificado.
pub fn write_dockerignore(path: &std::path::Path) -> Result<()> {
    std::fs::write(path, generate_dockerignore())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_contem_builder_stage() {
        let df = generate_dockerfile();
        assert!(df.contains("FROM rust:1.82-slim-bookworm AS builder"));
    }

    #[test]
    fn dockerfile_contem_entrypoint_arreio() {
        let df = generate_dockerfile();
        assert!(df.contains("ENTRYPOINT [\"arreio\"]"));
        assert!(df.contains("CMD [\"serve\"]"));
    }

    #[test]
    fn dockerfile_expose_8080() {
        let df = generate_dockerfile();
        assert!(df.contains("EXPOSE 8080"));
    }

    #[test]
    fn docker_compose_contem_servicos() {
        let dc = generate_docker_compose();
        assert!(dc.contains("services:"));
        assert!(dc.contains("arreio:"));
        assert!(dc.contains("ollama:"));
    }

    #[test]
    fn docker_compose_mapeia_portas() {
        let dc = generate_docker_compose();
        assert!(dc.contains("8080:8080"));
        assert!(dc.contains("11434:11434"));
    }

    #[test]
    fn dockerignore_ignora_target_e_git() {
        let di = generate_dockerignore();
        assert!(di.contains("target/"));
        assert!(di.contains(".git/"));
    }

    #[test]
    fn dockerignore_mantem_markdowns_importantes() {
        let di = generate_dockerignore();
        assert!(di.contains("!CLAUDE.md"));
        assert!(di.contains("!AGENTS.md"));
    }

    #[test]
    fn write_dockerfile_cria_arquivo_corretamente() {
        let tmpfile = tempfile::NamedTempFile::new().unwrap();
        write_dockerfile(tmpfile.path()).unwrap();
        let conteudo = std::fs::read_to_string(tmpfile.path()).unwrap();
        assert!(conteudo.contains("FROM rust:1.82-slim-bookworm AS builder"));
    }

    #[test]
    fn write_docker_compose_cria_arquivo_corretamente() {
        let tmpfile = tempfile::NamedTempFile::new().unwrap();
        write_docker_compose(tmpfile.path()).unwrap();
        let conteudo = std::fs::read_to_string(tmpfile.path()).unwrap();
        assert!(conteudo.contains("ollama/ollama:latest"));
    }

    #[test]
    fn write_dockerignore_cria_arquivo_corretamente() {
        let tmpfile = tempfile::NamedTempFile::new().unwrap();
        write_dockerignore(tmpfile.path()).unwrap();
        let conteudo = std::fs::read_to_string(tmpfile.path()).unwrap();
        assert!(conteudo.contains("*.md"));
    }
}
