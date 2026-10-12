use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct Message {
    pub statut: String,
    pub contenu: String,
}

#[derive(Clone, Serialize, ToSchema, FromRow)]
pub struct Hypervisor {
    pub id: String,
    pub nom: String,
    pub uri: String,
    pub statut: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateHypervisor {
    pub nom: String,
    pub uri: String,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateHypervisor {
    pub nom: Option<String>,
    pub uri: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateVm {
    pub nom: String,
    pub memoire_mb: u64,
    pub vcpus: u16,
}

#[derive(Deserialize, ToSchema)]
pub struct DiskRequest {
    pub nom: String,
    pub taille_gb: u64,
    pub format: String,
}
