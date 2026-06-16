#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    pub supports_thinking: bool,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    models: Vec<Model>,
}

impl Catalog {
    pub fn new() -> Self {
        let models = vec![
            Model {
                id: "anthropic/claude-sonnet-4".to_string(),
                name: "Claude Sonnet 4".to_string(),
                provider: "openrouter".to_string(),
                context_window: 200_000,
                supports_thinking: true,
            },
            // ... more models here later
        ];
        Self { models }
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    pub fn find(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.id == id )
    }
}

#[cfg(test)]
mod tests {

    use super::*;

   #[test]
   fn catalog_contains_at_least_one_model() {
       let catalog = Catalog::new();
       let has_models = !catalog.models().is_empty();
       assert!(has_models);
   }

   #[test]
   fn find_model_by_id_returns_model() {
    let catalog = Catalog::new();
    let id = "anthropic/claude-sonnet-4".to_string();
    let model = catalog.find(&id);
    assert!(model.is_some());
   }

   #[test]
   fn find_model_by_id_returns_none_if_not_exists() {
    let catalog = Catalog::new();
    let id = "z.ai/glm-5-turbo".to_string();
    let model = catalog.find(&id);
    assert!(model.is_none());
   }
}
