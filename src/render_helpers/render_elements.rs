#[macro_export]
macro_rules! bakawm_render_elements {
    ($name:ident<R> => { $($variant:ident = $type:ty),+ $(,)? }) => {
        $crate::bakawm_render_elements!(@impl $name () ($name<R>) => { $($variant = $type),+ });

        $(impl<R: smithay::backend::renderer::Renderer> From<$type> for $name<R> {
            fn from(x: $type) -> Self {
                Self::$variant(x)
            }
        })+
    };

    ($name:ident => { $($variant:ident = $type:ty),+ $(,)? }) => {
        $crate::bakawm_render_elements!(@impl $name ($name) () => { $($variant = $type),+ });

        $(impl From<$type> for $name {
            fn from(x: $type) -> Self {
                Self::$variant(x)
            }
        })+
    };

    (@impl $name:ident ($($name_no_R:ident)?) ($($name_R:ident<$R:ident>)?) => { $($variant:ident = $type:ty),+ }) => {
        #[allow(clippy::large_enum_variant)]
        #[derive(Debug)]
        pub enum $name$(<$R>)? {
            $($variant($type)),+
        }

        impl$(<$R>)? smithay::backend::renderer::element::Element for $name$(<$R>)? {
            fn id(&self) -> &smithay::backend::renderer::element::Id {
                match self {
                    $($name::$variant(elem) => elem.id()),+
                }
            }

            fn current_commit(&self) -> smithay::backend::renderer::utils::CommitCounter {
                match self {
                    $($name::$variant(elem) => elem.current_commit()),+
                }
            }

            fn geometry(&self, scale: smithay::utils::Scale<f64>) -> smithay::utils::Rectangle<i32, smithay::utils::Physical> {
                match self {
                    $($name::$variant(elem) => elem.geometry(scale)),+
                }
            }

            fn transform(&self) -> smithay::utils::Transform {
                match self {
                    $($name::$variant(elem) => elem.transform()),+
                }
            }

            fn src(&self) -> smithay::utils::Rectangle<f64, smithay::utils::Buffer> {
                match self {
                    $($name::$variant(elem) => elem.src()),+
                }
            }

            fn damage_since(
                &self,
                scale: smithay::utils::Scale<f64>,
                commit: Option<smithay::backend::renderer::utils::CommitCounter>,
            ) -> smithay::backend::renderer::utils::DamageSet<i32, smithay::utils::Physical> {
                match self {
                    $($name::$variant(elem) => elem.damage_since(scale, commit)),+
                }
            }

            fn opaque_regions(&self, scale: smithay::utils::Scale<f64>) -> smithay::backend::renderer::utils::OpaqueRegions<i32, smithay::utils::Physical> {
                match self {
                    $($name::$variant(elem) => elem.opaque_regions(scale)),+
                }
            }

            fn alpha(&self) -> f32 {
                match self {
                    $($name::$variant(elem) => elem.alpha()),+
                }
            }

            fn kind(&self) -> smithay::backend::renderer::element::Kind {
                match self {
                    $($name::$variant(elem) => elem.kind()),+
                }
            }
        }

        impl smithay::backend::renderer::element::RenderElement<smithay::backend::renderer::gles::GlesRenderer>
            for $($name_R<smithay::backend::renderer::gles::GlesRenderer>)? $($name_no_R)?
        {
            fn draw(
                &self,
                frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
                src: smithay::utils::Rectangle<f64, smithay::utils::Buffer>,
                dst: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
                damage: &[smithay::utils::Rectangle<i32, smithay::utils::Physical>],
                opaque_regions: &[smithay::utils::Rectangle<i32, smithay::utils::Physical>],
                cache: Option<&smithay::utils::user_data::UserDataMap>,
            ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
                match self {
                    $($name::$variant(elem) => {
                        smithay::backend::renderer::element::RenderElement::<smithay::backend::renderer::gles::GlesRenderer>::draw(elem, frame, src, dst, damage, opaque_regions, cache)
                    })+
                }
            }

            fn underlying_storage(&self, renderer: &mut smithay::backend::renderer::gles::GlesRenderer) -> Option<smithay::backend::renderer::element::UnderlyingStorage<'_>> {
                match self {
                    $($name::$variant(elem) => elem.underlying_storage(renderer)),+
                }
            }
        }
    };
}